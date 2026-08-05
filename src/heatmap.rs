//! `facet.heatmap` over `location`/`location_rpt` fields (#334).
//!
//! Solr's heatmap facet bins the documents matching the main query (and `fq`)
//! into a rectangular grid over a bounding geometry, emitting per-cell counts
//! as `counts_ints2D`. Wayfinder stores a `location`/`location_rpt` point as
//! two synthetic f64 fast columns (`__lat`/`__lon`, shared encoding with #331),
//! so the heatmap reads them back per matching document and tallies cells -- a
//! site-scale O(N) pass (ponytail: every matching doc is visited; Solr's RPT
//! walks a spatial term index, which Tantivy lacks -- see the #334 sizing
//! report and finding 159).
//!
//! Grid model (finding 159, verified cell-for-cell against every captured
//! fixture): the prefix tree is a geohash, and `gridLevel` is the geohash
//! character count, so `columns = 2^ceil(5L/2)` and `rows = 2^floor(5L/2)`
//! (longitude carries the odd bit). Cells are right-closed in longitude and
//! north-anchored in latitude (`row 0` is the top/north row). A bounded `geom`
//! subsets the world grid to the cells overlapping the box, snapped out to cell
//! edges. All-zero rows serialize as `null`; a wholly empty grid serializes as
//! a single `null`.

use crate::core_index::CoreIndex;
use crate::facet::{BaseClauses, base_query};
use crate::params::Params;
use crate::schema::ValueKind;
use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};
use tantivy::collector::Collector;
use tantivy::columnar::Column;
use tantivy::{DocId, Score, SegmentOrdinal, SegmentReader};

/// Geohash RPT world bounds (degrees).
const MIN_X: f64 = -180.0;
const MAX_X: f64 = 180.0;
const MIN_Y: f64 = -90.0;
const MAX_Y: f64 = 90.0;
/// Geohash carries 5 bits per level; 11 is the tree's `maxLevels`.
const MAX_LEVELS: u32 = 11;
/// Default `facet.heatmap.distErrPct` -- NOT the field type's `0.025`
/// (finding 159).
const DEFAULT_DIST_ERR_PCT: f64 = 0.1;
/// Default `facet.heatmap.maxCells`.
const DEFAULT_MAX_CELLS: u64 = 100_000;

// Cell assignment is plain `ceil((lon-MIN_X)/cell_w) - 1` (right-closed
// longitude) and `floor((MAX_Y-lat)/cell_h)` (north-anchored rows) with no FP
// nudge: the captured coordinates are integers and the geohash cell widths
// (45, 11.25, 5.625, ...) are exactly representable, so the divisions are
// exact for every fixture. A nudge large enough to absorb f64 round-off would
// also shift genuinely near-edge coordinates to the wrong cell, so none is
// applied; a future fixture exposing true round-off overshoot is where a
// carefully-sized tolerance gets added (ponytail).

#[derive(Clone, Copy, Debug)]
pub(crate) struct Rect {
    pub(crate) min_x: f64,
    pub(crate) max_x: f64,
    pub(crate) min_y: f64,
    pub(crate) max_y: f64,
}

impl Rect {
    fn world() -> Self {
        Rect {
            min_x: MIN_X,
            max_x: MAX_X,
            min_y: MIN_Y,
            max_y: MAX_Y,
        }
    }

    fn diagonal(&self) -> f64 {
        ((self.max_x - self.min_x).powi(2) + (self.max_y - self.min_y).powi(2)).sqrt()
    }

    /// Solr's `Rect.toString()`, embedded in the maxCells overflow message.
    fn to_rect_string(self) -> String {
        format!(
            "Rect(minX={},maxX={},minY={},maxY={})",
            fmt_deg(self.min_x),
            fmt_deg(self.max_x),
            fmt_deg(self.min_y),
            fmt_deg(self.max_y)
        )
    }
}

/// Formats a degree value the way Solr's `Rectangle.toString()` does: a plain
/// Java `double` (`-180.0`, `90.0`), not `-180`/`90`. `ryu` via serde happens
/// to match this for the JSON fields, but this message string is built by hand,
/// so format explicitly.
fn fmt_deg(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

/// `columns = 2^ceil(5L/2)`, `rows = 2^floor(5L/2)` (finding 159).
fn level_dims(level: u32) -> (usize, usize) {
    let bits = 5 * level;
    let columns = 1usize << bits.div_ceil(2);
    let rows = 1usize << (bits / 2);
    (columns, rows)
}

/// `columns * rows` at `level` = `2^(5L)` = `32^L` (the sum of ceil/floor of
/// `5L/2` is always `5L`).
fn cells_for_level(level: u32) -> u64 {
    1u64 << (5 * level)
}

/// The default grid level: the smallest level whose longitude cell width is
/// `<= distErrPct * diagonal` (finding 159). Both the whole world and the
/// captured 20x10 box resolve to level 2 under the default `0.1`.
fn level_for_dist_err(diagonal: f64, dist_err_pct: f64) -> u32 {
    let target = dist_err_pct * diagonal;
    (1..=MAX_LEVELS)
        .find(|&l| {
            let (cols, _) = level_dims(l);
            360.0 / cols as f64 <= target
        })
        .unwrap_or(MAX_LEVELS)
}

/// The resolved grid for one heatmap: level, the sub-grid's column/row range
/// within the world grid, cell sizes, and the snapped extents reported back.
#[derive(Clone, Copy)]
struct Grid {
    level: u32,
    columns_full: usize,
    rows_full: usize,
    out_columns: usize,
    out_rows: usize,
    first_col: usize,
    last_col: usize,
    first_row: usize,
    last_row: usize,
    cell_w: f64,
    cell_h: f64,
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
}

impl Grid {
    fn new(level: u32, geom: Rect) -> Grid {
        let (cols, rows) = level_dims(level);
        let cell_w = 360.0 / cols as f64;
        let cell_h = 180.0 / rows as f64;
        // World cell of a longitude, clamped to the world grid, for the geom
        // bounds (a box may run past the world edges).
        let col_of = |lon: f64| -> usize {
            let c = (((lon - MIN_X) / cell_w).ceil() as i64 - 1).max(0) as usize;
            c.min(cols - 1)
        };
        // World row of a latitude (row 0 = north), clamped.
        let row_of = |lat: f64| -> usize {
            let r = (((MAX_Y - lat) / cell_h).floor() as i64).max(0) as usize;
            r.min(rows - 1)
        };
        let first_col = col_of(geom.min_x);
        let last_col = col_of(geom.max_x);
        let first_row = row_of(geom.max_y);
        let last_row = row_of(geom.min_y);
        Grid {
            level,
            columns_full: cols,
            rows_full: rows,
            out_columns: last_col - first_col + 1,
            out_rows: last_row - first_row + 1,
            first_col,
            last_col,
            first_row,
            last_row,
            cell_w,
            cell_h,
            min_x: MIN_X + first_col as f64 * cell_w,
            max_x: MIN_X + (last_col + 1) as f64 * cell_w,
            max_y: MAX_Y - first_row as f64 * cell_h,
            min_y: MAX_Y - (last_row + 1) as f64 * cell_h,
        }
    }

    /// World column of a longitude (right-closed cells), or `None` if the
    /// longitude is outside the world grid (e.g. exactly `MIN_X`, the open
    /// west edge, or a non-finite value).
    fn world_col(&self, lon: f64) -> Option<usize> {
        if !lon.is_finite() {
            return None;
        }
        let c = ((lon - MIN_X) / self.cell_w).ceil() as i64 - 1;
        if (0..self.columns_full as i64).contains(&c) {
            Some(c as usize)
        } else {
            None
        }
    }

    /// World row of a latitude (row 0 = north), or `None` if outside the grid.
    fn world_row(&self, lat: f64) -> Option<usize> {
        if !lat.is_finite() {
            return None;
        }
        let r = ((MAX_Y - lat) / self.cell_h).floor() as i64;
        if (0..self.rows_full as i64).contains(&r) {
            Some(r as usize)
        } else {
            None
        }
    }

    /// `(out_row, out_col)` of a point if it lands inside this grid's
    /// sub-rectangle; `None` otherwise (outside the geom or off the world).
    fn cell_of(&self, lat: f64, lon: f64) -> Option<(usize, usize)> {
        let col = self.world_col(lon)?;
        let row = self.world_row(lat)?;
        if col < self.first_col
            || col > self.last_col
            || row < self.first_row
            || row > self.last_row
        {
            return None;
        }
        Some((row - self.first_row, col - self.first_col))
    }
}

/// Parses the rectangle form Solr's RPT accepts for `facet.heatmap.geom` and
/// the `<field>:<geom>` `fq`: `["minLon minLat" TO "maxLon maxLat"]` (lon
/// before lat inside each corner, the opposite of the `lat,lon` point form).
/// WKT/circle geoms are a descope (no fixture exercises them); this returns a
/// clear error naming the supported form rather than silently treating them as
/// the world.
pub(crate) fn parse_geom(s: &str) -> Result<Rect> {
    let s = s.trim();
    if !(s.starts_with('[') && s.ends_with(']')) {
        bail!(
            "facet.heatmap.geom only supports the rectangle `[\"minLon minLat\" TO \"maxLon \
             maxLat\"]` form; got {s:?} (WKT/circle is not implemented)"
        );
    }
    let inner = &s[1..s.len() - 1];
    let (lower, upper) = inner.split_once(" TO ").ok_or_else(|| {
        anyhow!("facet.heatmap.geom rectangle needs ` TO ` between its two corners; got {s:?}")
    })?;
    let (lo_lon, lo_lat) = parse_corner(lower)?;
    let (hi_lon, hi_lat) = parse_corner(upper)?;
    // Solr normalizes the Rect so min <= max on each axis.
    Ok(Rect {
        min_x: lo_lon.min(hi_lon),
        max_x: lo_lon.max(hi_lon),
        min_y: lo_lat.min(hi_lat),
        max_y: lo_lat.max(hi_lat),
    })
}

/// `params.get(key)` parsed as u64, erroring on a present-but-non-numeric
/// value (a client typo) and returning `None` when the param is absent.
fn parse_opt_u64(params: &Params, key: &str) -> Result<Option<u64>> {
    match params.get(key) {
        None => Ok(None),
        Some(s) => Ok(Some(s.parse::<u64>().map_err(|_| {
            anyhow!("{key} must be a non-negative integer, got {s:?}")
        })?)),
    }
}

/// Same as [`parse_opt_u64`] for an f64.
fn parse_opt_f64(params: &Params, key: &str) -> Result<Option<f64>> {
    match params.get(key) {
        None => Ok(None),
        Some(s) => {
            Ok(Some(s.parse::<f64>().map_err(|_| {
                anyhow!("{key} must be a number, got {s:?}")
            })?))
        }
    }
}

fn parse_corner(s: &str) -> Result<(f64, f64)> {
    let s = s.trim().trim_matches('"').trim();
    let mut it = s.split_whitespace();
    let lon = it
        .next()
        .ok_or_else(|| anyhow!("facet.heatmap.geom corner needs `lon lat`, got {s:?}"))?
        .parse::<f64>()
        .map_err(|_| anyhow!("facet.heatmap.geom corner longitude is not a number: {s:?}"))?;
    let lat = it
        .next()
        .ok_or_else(|| anyhow!("facet.heatmap.geom corner needs `lon lat`, got {s:?}"))?
        .parse::<f64>()
        .map_err(|_| anyhow!("facet.heatmap.geom corner latitude is not a number: {s:?}"))?;
    Ok((lon, lat))
}

// --- the collector: tally per-cell counts over the matching doc set ---------

/// A `Collector` that reads each matching document's `__lat`/`__lon` fast
/// columns and increments the cell it falls in. The grid spec is fixed up
/// front (level/geom/maxCells are request params, not per-doc), so the segment
/// collectors just need the two column readers and a flat counts buffer.
pub(crate) struct HeatmapCollector {
    lat_name: String,
    lon_name: String,
    grid: Grid,
}

pub(crate) struct HeatmapSegmentCollector {
    lat: Option<Column<f64>>,
    lon: Option<Column<f64>>,
    grid: Grid,
    counts: Vec<i64>,
}

impl Collector for HeatmapCollector {
    type Fruit = Vec<i64>;
    type Child = HeatmapSegmentCollector;

    fn for_segment(
        &self,
        _segment_ord: SegmentOrdinal,
        segment: &SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        let fast = segment.fast_fields();
        let lat = fast.column_opt::<f64>(&self.lat_name)?;
        let lon = fast.column_opt::<f64>(&self.lon_name)?;
        Ok(HeatmapSegmentCollector {
            lat,
            lon,
            grid: self.grid,
            counts: vec![0i64; self.grid.out_rows * self.grid.out_columns],
        })
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(&self, fruits: Vec<Self::Fruit>) -> tantivy::Result<Self::Fruit> {
        let len = self.grid.out_rows * self.grid.out_columns;
        let mut it = fruits.into_iter();
        let mut acc = it.next().unwrap_or_else(|| vec![0; len]);
        for f in it {
            for (a, b) in acc.iter_mut().zip(f.iter()) {
                *a += b;
            }
        }
        Ok(acc)
    }
}

impl tantivy::collector::SegmentCollector for HeatmapSegmentCollector {
    type Fruit = Vec<i64>;

    fn collect(&mut self, doc: DocId, _score: Score) {
        // A static location field always has both columns; `None` only for a
        // segment written before the field existed (defensive -- no counts).
        let (Some(lat_col), Some(lon_col)) = (self.lat.as_ref(), self.lon.as_ref()) else {
            return;
        };
        let Some(lat) = lat_col.values_for_doc(doc).next() else {
            return;
        };
        let Some(lon) = lon_col.values_for_doc(doc).next() else {
            return;
        };
        if let Some((row, col)) = self.grid.cell_of(lat, lon) {
            self.counts[row * self.grid.out_columns + col] += 1;
        }
    }

    fn harvest(self) -> Self::Fruit {
        self.counts
    }
}

// --- the entry point --------------------------------------------------------

/// Builds the `facet_heatmaps` object for a `/select` response (#334). One
/// entry per `facet.heatmap=<field>`, sharing the global
/// `facet.heatmap.gridLevel`/`.geom`/`.maxCells`/`.distErrPct` params. Every
/// error (undefined field, non-spatial field, maxCells overflow) is a 400 via
/// the `PreQueryFacetError` wrap in `facet_counts_inner`.
pub fn facet_heatmaps(index: &CoreIndex, params: &Params, base: &BaseClauses) -> Result<Value> {
    let mut out = Map::new();
    let fields: Vec<String> = params
        .get_all("facet.heatmap")
        .into_iter()
        .map(str::to_string)
        .collect();
    if fields.is_empty() {
        return Ok(Value::Object(out));
    }
    let explicit_level = params.get("facet.heatmap.gridLevel").map(str::to_string);
    let geom_str = params.get("facet.heatmap.geom");
    // A present-but-non-numeric param is a client error (Solr 400s too), not a
    // reason to silently fall back to the default -- that would mask the typo.
    let max_cells = parse_opt_u64(params, "facet.heatmap.maxCells")?.unwrap_or(DEFAULT_MAX_CELLS);
    let dist_err_pct =
        parse_opt_f64(params, "facet.heatmap.distErrPct")?.unwrap_or(DEFAULT_DIST_ERR_PCT);
    // `facet.heatmap.distErr` (absolute, the distErrPct alternative) is
    // accepted for strict_params parity but not implemented: if sent it is
    // ignored and the level is distErrPct-derived (or explicit) as usual. A
    // descope, not a silent wrong answer, since distErr only changes the
    // *default* level and an explicit gridLevel overrides it.
    // `facet.heatmap.format` is accepted but only `ints2D` (the default) is
    // implemented; `png` is a descope.
    for field in &fields {
        out.insert(
            field.clone(),
            one_heatmap(
                index,
                base,
                field,
                explicit_level.as_deref(),
                geom_str,
                max_cells,
                dist_err_pct,
            )?,
        );
    }
    Ok(Value::Object(out))
}

#[allow(clippy::too_many_arguments)]
fn one_heatmap(
    index: &CoreIndex,
    base: &BaseClauses,
    field: &str,
    explicit_level: Option<&str>,
    geom_str: Option<&str>,
    max_cells: u64,
    dist_err_pct: f64,
) -> Result<Value> {
    // The field must exist and be a location field. Solr's two messages diverge
    // in Java class names; Wayfinder emits functional equivalents (the
    // fixture normaliser strips `error.msg`, so only the 400 status is
    // compared there -- finding 159).
    let kind = index
        .wf_schema
        .value_kind(field)
        .ok_or_else(|| anyhow!("undefined field: \"{field}\""))?;
    if kind != ValueKind::Location {
        bail!(
            "heatmap field needs to be of type SpatialRecursivePrefixTreeFieldType or \
             RptWithGeometrySpatialField; \"{field}\" is not a spatial field, path=facet/"
        );
    }
    // Only a declared static location field has the synthetic columns; a dynamic
    // `rpts_*` value lives in the catch-all JSON and cannot be bucketed (a
    // documented ceiling, consistent with `geodist()` on a dynamic field).
    index.wf_schema.location_fields(field).ok_or_else(|| {
        anyhow!(
            "facet.heatmap on `{field}` needs a declared static field, not a dynamic one \
             (dynamic `rpts_*`/`locs_*` points have no fast columns)"
        )
    })?;

    let geom = match geom_str {
        Some(s) => parse_geom(s)?,
        None => Rect::world(),
    };

    let level = match explicit_level {
        Some(s) => {
            let l = s
                .parse::<u32>()
                .map_err(|_| anyhow!("facet.heatmap.gridLevel must be an integer, got {s:?}"))?;
            if !(1..=MAX_LEVELS).contains(&l) {
                bail!("facet.heatmap.gridLevel must be in 1..={MAX_LEVELS}, got {l}");
            }
            l
        }
        // With no explicit gridLevel the level is purely distErrPct-derived
        // (finding 159). maxCells is a ceiling guard only -- it never lowers
        // the level -- so the default level is not capped to fit it; the
        // overflow guard below 400s instead. (The default level under the
        // default maxCells is 2 = 1024 cells, far under 100000.)
        None => level_for_dist_err(geom.diagonal(), dist_err_pct),
    };

    // maxCells is a ceiling guard on the level's full grid (finding 159): it
    // never lowers an explicit level, it 400s.
    if cells_for_level(level) > max_cells {
        let (cols, rows) = level_dims(level);
        bail!(
            "Too many cells ({cols} x {rows}) for level {level} shape {}",
            geom.to_rect_string()
        );
    }

    let grid = Grid::new(level, geom);

    // Re-resolve the column pair now that the field is known to be a static
    // location field; build the synthetic names the columns were stored under.
    let (lat_field, lon_field) = index
        .wf_schema
        .location_fields(field)
        .expect("checked Location + static above");
    let lat_name = index
        .wf_schema
        .tantivy_schema
        .get_field_name(lat_field)
        .to_string();
    let lon_name = index
        .wf_schema
        .tantivy_schema
        .get_field_name(lon_field)
        .to_string();
    let collector = HeatmapCollector {
        lat_name,
        lon_name,
        grid,
    };
    let counts = index.collect(&base_query(base), &collector)?;
    Ok(render_heatmap(grid, &counts))
}

/// Renders one field's heatmap object: gridLevel/columns/rows/extents always
/// present; `counts_ints2D` is the row array (all-zero rows -> `null`) or a
/// single `null` when the whole grid is empty (`heatmap_empty`).
fn render_heatmap(grid: Grid, counts: &[i64]) -> Value {
    let counts_ints2d = if counts.iter().all(|&c| c == 0) {
        Value::Null
    } else {
        let mut rows = Vec::with_capacity(grid.out_rows);
        for r in 0..grid.out_rows {
            let row = &counts[r * grid.out_columns..(r + 1) * grid.out_columns];
            if row.iter().all(|&c| c == 0) {
                rows.push(Value::Null);
            } else {
                rows.push(Value::Array(row.iter().map(|&c| json!(c)).collect()));
            }
        }
        Value::Array(rows)
    };
    let mut m = Map::new();
    m.insert("gridLevel".into(), json!(grid.level));
    m.insert("columns".into(), json!(grid.out_columns));
    m.insert("rows".into(), json!(grid.out_rows));
    m.insert("minX".into(), json!(grid.min_x));
    m.insert("maxX".into(), json!(grid.max_x));
    m.insert("minY".into(), json!(grid.min_y));
    m.insert("maxY".into(), json!(grid.max_y));
    m.insert("counts_ints2D".into(), counts_ints2d);
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_dims_are_2_to_the_geohash_bit_split() {
        // columns = 2^ceil(5L/2), rows = 2^floor(5L/2); verified against every
        // captured fixture's reported columns/rows (finding 159).
        assert_eq!(level_dims(1), (8, 4));
        assert_eq!(level_dims(2), (32, 32));
        assert_eq!(level_dims(3), (256, 128));
        assert_eq!(level_dims(4), (1024, 1024));
    }

    #[test]
    fn cells_for_level_is_32_to_the_l() {
        assert_eq!(cells_for_level(1), 32);
        assert_eq!(cells_for_level(2), 1_024);
        assert_eq!(cells_for_level(3), 32_768);
        assert_eq!(cells_for_level(4), 1_048_576);
    }

    #[test]
    fn default_level_is_dist_err_pct_times_diagonal_world_and_box_both_2() {
        // Whole world -> level 2 (heatmap_default_world); the 20x10 box ->
        // level 2 too (heatmap_default_bounded). distErrPct default 0.1.
        let world = Rect::world().diagonal();
        assert_eq!(level_for_dist_err(world, 0.1), 2);
        let box_diag = Rect {
            min_x: -90.0,
            max_x: 90.0,
            min_y: -45.0,
            max_y: 45.0,
        }
        .diagonal();
        assert_eq!(level_for_dist_err(box_diag, 0.1), 2);
    }

    #[test]
    fn world_grid_l1_extents_and_point_cells_match_fixture() {
        // heatmap_l1_world: 8x4 over the world. Each of the 10 corpus points
        // (verified cell-for-cell against the captured counts_ints2D).
        let g = Grid::new(1, Rect::world());
        assert_eq!((g.out_columns, g.out_rows), (8, 4));
        assert_eq!(
            (g.min_x, g.max_x, g.min_y, g.max_y),
            (-180.0, 180.0, -90.0, 90.0)
        );
        // (lat, lon) -> (out_row, out_col).
        let cell = |lat: f64, lon: f64| g.cell_of(lat, lon);
        assert_eq!(cell(10.0, 20.0), Some((1, 4))); // h1
        assert_eq!(cell(10.0, 22.0), Some((1, 4))); // h2 (same cell as h1)
        assert_eq!(cell(12.0, 20.0), Some((1, 4))); // h3 (same cell)
        assert_eq!(cell(45.0, 45.0), Some((1, 4))); // h10 (the 4th in that cell)
        assert_eq!(cell(35.0, -75.0), Some((1, 2))); // h7
        assert_eq!(cell(5.0, 130.0), Some((1, 6))); // h8
        assert_eq!(cell(60.0, 140.0), Some((0, 7))); // h5 (north row)
        assert_eq!(cell(0.0, 0.0), Some((2, 3))); // h9 (boundary lon=0 -> col 3)
        assert_eq!(cell(-20.0, -100.0), Some((2, 1))); // h4
        assert_eq!(cell(-70.0, 30.0), Some((3, 4))); // h6 (south row)
    }

    #[test]
    fn bounded_grid_l2_snaps_out_to_cell_edges() {
        // heatmap_l2_bounded: geom lon[-90,90] lat[-45,45] at level 2.
        let g = Grid::new(
            2,
            Rect {
                min_x: -90.0,
                max_x: 90.0,
                min_y: -45.0,
                max_y: 45.0,
            },
        );
        assert_eq!((g.out_columns, g.out_rows), (17, 17));
        assert_eq!((g.first_col, g.last_col), (7, 23));
        assert_eq!((g.first_row, g.last_row), (8, 24));
        assert_eq!((g.min_x, g.max_x), (-101.25, 90.0));
        assert_eq!((g.min_y, g.max_y), (-50.625, 45.0));
        // h6 (lat -70) is outside the geom (lat < -45) -> not in the grid.
        assert_eq!(g.cell_of(-70.0, 30.0), None);
        // h10 (45,45) is the top-right corner cell.
        assert_eq!(g.cell_of(45.0, 45.0), Some((0, 12)));
        // h1 (10,20) world cell (col17,row14) -> out (col10,row6).
        assert_eq!(g.cell_of(10.0, 20.0), Some((6, 10)));
    }

    #[test]
    fn parse_geom_rectangle_reads_lon_then_lat_and_normalizes() {
        let r = parse_geom(r#"["-90 -45" TO "90 45"]"#).unwrap();
        assert_eq!(
            (r.min_x, r.max_x, r.min_y, r.max_y),
            (-90.0, 90.0, -45.0, 45.0)
        );
        // Corners swapped come out normalized (Solr's Rect is min-anchored).
        let r = parse_geom(r#"["90 45" TO "-90 -45"]"#).unwrap();
        assert_eq!(
            (r.min_x, r.max_x, r.min_y, r.max_y),
            (-90.0, 90.0, -45.0, 45.0)
        );
    }

    #[test]
    fn parse_geom_rejects_non_rectangle_shapes() {
        // WKT / circle are a descope; the error names the supported form.
        assert!(parse_geom("ENVELOPE(-90,90,45,-45)").is_err());
        assert!(parse_geom("POLYGON((0 0,1 0,1 1,0 1,0 0))").is_err());
    }
}
