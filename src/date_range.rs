//! `solr.DateRangeField` (issue #341): interval-valued dates with truncated
//! literals, date math, and the `Intersects`/`Contains`/`Within` spatial-style
//! predicates.
//!
//! Ground truth is `solr-ref/responses/dr341_*.json` and findings 165-172 in
//! `docs/solr-ref-findings.md`. The four rules this module exists to enforce:
//!
//! - **166** every literal denotes the whole interval of its stated precision,
//!   at millisecond resolution, end-INCLUSIVE — and that applies to interval
//!   *endpoints* too, not just bare literals: `[2020-03 TO 2020-09]` ends at
//!   `2020-09-30T23:59:59.999Z`.
//! - **167** `Intersects` is the default op, `IsWithin` aliases `Within`, and
//!   `op` is matched case-insensitively. `Contains` and `Within` are NOT
//!   complements.
//! - **168** a multiValued field is ONE point set: the union of its members,
//!   holes included. `Within` collapses to `min(start) >= qStart AND max(end)
//!   <= qEnd`, but `Intersects` and `Contains` are hole-sensitive and must be
//!   evaluated member by member — "any member matches" is wrong for `Within`
//!   and "the min/max span matches" is wrong for the other two.
//! - **170** the error split is by failure KIND: an unparseable value is a 400
//!   ([`DateRangeError::Parse`]), a valid-but-unimplemented op or a reversed
//!   interval is a 500 ([`DateRangeError::Unsupported`]). The `msg` strings
//!   here are pinned verbatim by the fixtures.

use std::fmt;

use tantivy::columnar::Column;
use tantivy::query::{EnableScoring, Explanation, Query, Scorer, Weight};
use tantivy::time::format_description::well_known::Rfc3339;
use tantivy::time::{Date, Duration, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};
use tantivy::{DateTime, DocId, DocSet, Score, SegmentReader, TERMINATED, Term};

/// The millisecond timestamp an open lower bound (`*`) resolves to, and the
/// floor of every interval this module can represent.
///
/// ponytail: NOT Solr's `0001-01-01T00:00:00Z`. `tantivy::DateTime` stores an
/// `i64` of NANOseconds, so its whole representable range is roughly
/// 1677-09-21 .. 2262-04-11 — year 1 does not fit, and constructing it
/// overflows. These two constants are that range rounded inwards to a whole
/// millisecond, so `[* TO *]` means "every instant Tantivy can store" rather
/// than "every instant Solr can name". Ceiling: an interval endpoint outside
/// 1678..2261 is not representable at all, and a query for one is answered
/// against the clamped bound. No fixture goes anywhere near it (the corpus
/// spans 2019..2022).
pub const MIN_MS: i64 = i64::MIN / 1_000_000 + 1;
/// The millisecond timestamp an open upper bound (`*`) resolves to. See
/// [`MIN_MS`] for why this is not Solr's `9999-12-31T23:59:59.999Z`.
pub const MAX_MS: i64 = i64::MAX / 1_000_000;

/// A closed, end-inclusive millisecond interval — the only shape a
/// `date_range` value or query ever has (finding 169: exclusive-brace syntax
/// is accepted and silently ignored, so there is no exclusive endpoint to
/// model).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval {
    pub start_ms: i64,
    pub end_ms: i64,
}

/// A `date_range` failure, split by the kind finding 170 pins to a status
/// code. The `Display` text is the wire `msg`, verbatim.
#[derive(Debug)]
pub enum DateRangeError {
    /// The value could not be parsed at all -> HTTP 400.
    Parse(String),
    /// The value parsed but the type cannot answer it (a reversed interval, an
    /// operation `DateRangeField` does not implement) -> HTTP 500. Solr leaks
    /// an exception here; the fixtures pin its bare `msg`.
    Unsupported(String),
}

impl fmt::Display for DateRangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DateRangeError::Parse(msg) | DateRangeError::Unsupported(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for DateRangeError {}

type DrResult<T> = Result<T, DateRangeError>;

/// The three set relations `DateRangeField` actually implements (finding 167).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Intersects,
    Contains,
    Within,
}

/// Resolves Lucene's `SpatialOperation` name (or alias) for an `op=` value,
/// case-insensitively (finding 167, `dr341_op_lowercase`).
///
/// Every name Lucene knows but `DateRangeField` does not implement is a 500
/// carrying that operation's own bare name (`dr341_err_disjoint` -> `Disjoint`,
/// `dr341_err_overlaps` -> `Overlaps`, `dr341_err_equals` -> `Equals`), and a
/// name Lucene does not know at all is a 500 too, `Unknown Operation: <value>`
/// with the value exactly as sent (`dr341_err_bad_op`).
///
/// ponytail: `BBoxIntersects`/`BBoxWithin` are real `SpatialOperation` names
/// that no fixture exercises. They are answered with the same
/// unimplemented-operation 500 as `Overlaps`, on the reasoning that
/// `DateRangeField` implements exactly the three ops above; if a capture ever
/// shows Solr treating a BBox op as an alias of `Intersects`/`Within`, this is
/// the arm to move.
pub fn parse_op(raw: &str) -> DrResult<Op> {
    match raw.to_ascii_lowercase().as_str() {
        "intersects" => Ok(Op::Intersects),
        "contains" => Ok(Op::Contains),
        "within" | "iswithin" => Ok(Op::Within),
        "isdisjointto" | "disjoint" => Err(DateRangeError::Unsupported("Disjoint".to_string())),
        "overlaps" => Err(DateRangeError::Unsupported("Overlaps".to_string())),
        "equals" | "isequalto" => Err(DateRangeError::Unsupported("Equals".to_string())),
        "bboxintersects" => Err(DateRangeError::Unsupported("BBoxIntersects".to_string())),
        "bboxwithin" => Err(DateRangeError::Unsupported("BBoxWithin".to_string())),
        _ => Err(DateRangeError::Unsupported(format!(
            "Unknown Operation: {raw}"
        ))),
    }
}

/// Parses a `date_range` value or query text into the interval it denotes:
/// either `[a TO b]` / `{a TO b}` (finding 169 — the brace form is accepted
/// and behaves identically), or a bare literal, which is the whole interval of
/// its own precision (finding 166).
///
/// A reversed interval is the finding-170 500, quoting the two endpoint tokens
/// exactly as written (`Wrong order: 2021 TO 2020`).
pub fn parse_interval(text: &str) -> DrResult<Interval> {
    let s = text.trim();
    let inner = s
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .or_else(|| s.strip_prefix('{').and_then(|rest| rest.strip_suffix('}')))
        // A mixed pair (`[a TO b}`) is what Solr's own parser accepts too: it
        // looks at the first and last character independently.
        .or_else(|| s.strip_prefix('[').and_then(|rest| rest.strip_suffix('}')))
        .or_else(|| s.strip_prefix('{').and_then(|rest| rest.strip_suffix(']')));
    let Some(inner) = inner else {
        return endpoint_interval(s);
    };
    // ponytail: ` TO ` only, uppercase, exactly as Solr's `DateRangeField`
    // spells it. A lowercase `to` inside a bracketed value falls through to
    // the whole-string literal parse and so becomes the "improperly formatted
    // datetime" 400, which is also what Solr answers for it.
    let (lo, hi) = match inner.split_once(" TO ") {
        Some(pair) => pair,
        None => return Err(bad_datetime(s)),
    };
    let (lo, hi) = (lo.trim(), hi.trim());
    let start = endpoint_interval(lo)?.start_ms;
    let end = endpoint_interval(hi)?.end_ms;
    if start > end {
        return Err(DateRangeError::Unsupported(format!(
            "Wrong order: {lo} TO {hi}"
        )));
    }
    Ok(Interval {
        start_ms: start,
        end_ms: end,
    })
}

/// The interval one endpoint token denotes. `*` is the open bound; anything
/// beginning `NOW` is date math (finding 171); everything else is a truncated
/// or full date literal (finding 166).
fn endpoint_interval(token: &str) -> DrResult<Interval> {
    let t = token.trim();
    if t == "*" {
        return Ok(Interval {
            start_ms: MIN_MS,
            end_ms: MAX_MS,
        });
    }
    if t.starts_with("NOW") {
        let ms = date_math(t)?;
        // A date-math expression resolves to an instant, not a precision
        // bucket: `NOW/YEAR` is the first millisecond of this year, not the
        // whole year. `dr341_datemath_year`/`dr341_datemath_now` do not
        // discriminate between the two readings (both windows are open at the
        // far end relative to the corpus), so this is the smaller claim.
        return Ok(Interval {
            start_ms: ms,
            end_ms: ms,
        });
    }
    literal_interval(t)
}

fn bad_datetime(token: &str) -> DateRangeError {
    // Verbatim from `dr341_err_bad_date`: `Couldn't parse date because:
    // Improperly formatted datetime: 2020-13`.
    DateRangeError::Parse(format!(
        "Couldn't parse date because: Improperly formatted datetime: {token}"
    ))
}

/// Expands a truncated or full date literal into the whole interval of its
/// stated precision, end-inclusive at millisecond resolution (finding 166):
/// `2020` -> `[2020-01-01T00:00:00.000Z, 2020-12-31T23:59:59.999Z]`,
/// `2020-06-15T12:00:00Z` -> that whole second.
///
/// ponytail: UTC only — a trailing `Z` is optional and any other offset is a
/// 400. Solr's `DateRangeField` is UTC-only on the wire too (`Z` is the only
/// suffix its own parser accepts), so this is a limit on the *error* wording
/// rather than on accepted values.
fn literal_interval(token: &str) -> DrResult<Interval> {
    let body = token.strip_suffix('Z').unwrap_or(token);
    let err = || bad_datetime(token);

    // `YYYY[-MM[-DD[THH[:MM[:SS[.mmm]]]]]]`, each level narrowing the interval.
    let (date_part, time_part) = match body.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (body, None),
    };
    let mut date_fields = date_part.split('-');
    let year: i32 = parse_num(date_fields.next(), 4, err)?;
    let month_raw = date_fields.next();
    let day_raw = date_fields.next();
    if date_fields.next().is_some() {
        return Err(err());
    }
    if month_raw.is_none() && (day_raw.is_some() || time_part.is_some()) {
        return Err(err());
    }

    let month_num: u32 = match month_raw {
        None => 1,
        Some(m) => parse_num(Some(m), 2, err)?,
    };
    let month = Month::try_from(u8::try_from(month_num).map_err(|_| err())?).map_err(|_| err())?;
    let day: u32 = match day_raw {
        None => 1,
        Some(d) => parse_num(Some(d), 2, err)?,
    };
    if day_raw.is_none() && time_part.is_some() {
        return Err(err());
    }

    let (hour, minute, second, milli, precision) = match time_part {
        None => (0, 0, 0, 0, Precision::Day),
        Some(t) => {
            let mut parts = t.split(':');
            let hour: u32 = parse_num(parts.next(), 2, err)?;
            let minute_raw = parts.next();
            let second_raw = parts.next();
            if parts.next().is_some() {
                return Err(err());
            }
            if minute_raw.is_none() && second_raw.is_some() {
                return Err(err());
            }
            let minute: u32 = match minute_raw {
                None => 0,
                Some(m) => parse_num(Some(m), 2, err)?,
            };
            let (second, milli, precision) = match second_raw {
                None => (0, 0, None),
                Some(s) => match s.split_once('.') {
                    None => (
                        parse_num::<u32>(Some(s), 2, err)?,
                        0,
                        Some(Precision::Second),
                    ),
                    Some((whole, frac)) => {
                        if frac.is_empty()
                            || frac.len() > 9
                            || !frac.bytes().all(|b| b.is_ascii_digit())
                        {
                            return Err(err());
                        }
                        // Sub-millisecond digits are truncated: the whole type
                        // works at millisecond resolution (finding 166).
                        let mut digits = frac.to_string();
                        digits.truncate(3);
                        while digits.len() < 3 {
                            digits.push('0');
                        }
                        (
                            parse_num::<u32>(Some(whole), 2, err)?,
                            digits.parse::<u32>().map_err(|_| err())?,
                            Some(Precision::Milli),
                        )
                    }
                },
            };
            let precision = precision.unwrap_or(if minute_raw.is_some() {
                Precision::Minute
            } else {
                Precision::Hour
            });
            (hour, minute, second, milli, precision)
        }
    };

    let precision = match (month_raw, day_raw, time_part) {
        (None, _, _) => Precision::Year,
        (Some(_), None, _) => Precision::Month,
        _ => precision,
    };

    let date = Date::from_calendar_date(year, month, u8::try_from(day).map_err(|_| err())?)
        .map_err(|_| err())?;
    let time = Time::from_hms_milli(
        u8::try_from(hour).map_err(|_| err())?,
        u8::try_from(minute).map_err(|_| err())?,
        u8::try_from(second).map_err(|_| err())?,
        u16::try_from(milli).map_err(|_| err())?,
    )
    .map_err(|_| err())?;
    let start = PrimitiveDateTime::new(date, time).assume_utc();
    let start_ms = to_millis(start).ok_or_else(err)?;
    let end_ms = precision_end_ms(start, precision).ok_or_else(err)?;
    Ok(Interval { start_ms, end_ms })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Precision {
    Year,
    Month,
    Day,
    Hour,
    Minute,
    Second,
    Milli,
}

/// The last millisecond of the interval a literal of this precision denotes:
/// one millisecond before the same instant advanced by one unit of that
/// precision. Calendar units (year, month) advance by calendar arithmetic, so
/// February and leap years come out right.
fn precision_end_ms(start: OffsetDateTime, precision: Precision) -> Option<i64> {
    let next = match precision {
        Precision::Milli => return to_millis(start),
        Precision::Year => {
            let date = Date::from_calendar_date(start.year() + 1, Month::January, 1).ok()?;
            PrimitiveDateTime::new(date, Time::MIDNIGHT).assume_utc()
        }
        Precision::Month => {
            let (year, month) = if start.month() == Month::December {
                (start.year() + 1, Month::January)
            } else {
                (start.year(), start.month().next())
            };
            let date = Date::from_calendar_date(year, month, 1).ok()?;
            PrimitiveDateTime::new(date, Time::MIDNIGHT).assume_utc()
        }
        Precision::Day => start.checked_add(Duration::days(1))?,
        Precision::Hour => start.checked_add(Duration::hours(1))?,
        Precision::Minute => start.checked_add(Duration::minutes(1))?,
        Precision::Second => start.checked_add(Duration::seconds(1))?,
    };
    to_millis(next)?.checked_sub(1)
}

fn to_millis(dt: OffsetDateTime) -> Option<i64> {
    let nanos = dt.unix_timestamp_nanos();
    let millis = nanos.div_euclid(1_000_000);
    i64::try_from(millis)
        .ok()
        .filter(|ms| (MIN_MS..=MAX_MS).contains(ms))
}

fn parse_num<T: std::str::FromStr>(
    raw: Option<&str>,
    width: usize,
    err: impl Fn() -> DateRangeError,
) -> DrResult<T> {
    let raw = raw.ok_or_else(&err)?;
    if raw.len() != width || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(err());
    }
    raw.parse::<T>().map_err(|_| err())
}

/// Solr date math on `NOW` (finding 171): `NOW`, `NOW/DAY`, `NOW-2YEARS`,
/// `NOW/YEAR+1YEAR`, `NOW/DAY+1MONTH`. `/UNIT` rounds down to that unit;
/// `+`/`-<n><UNIT>` shifts. Returns the resolved instant in milliseconds.
///
/// An unparseable expression is the finding-170 400, quoting the whole
/// expression: `Invalid Date Math String:'NOW/BOGUS'` (`dr341_err_bad_math`).
///
/// ponytail: `NOW` is the only anchor — Solr also accepts date math suffixed
/// to an explicit `...Z` literal, and a `TZ` request param that shifts what
/// rounding means. Neither is client-exercised in any capture, and both would
/// need their own fixtures to pin. Rounding here is always UTC.
fn date_math(expr: &str) -> DrResult<i64> {
    let err = || DateRangeError::Parse(format!("Invalid Date Math String:'{expr}'"));
    let mut now = OffsetDateTime::now_utc().to_offset(UtcOffset::UTC);
    let mut rest = expr.strip_prefix("NOW").ok_or_else(&err)?;
    while !rest.is_empty() {
        let (op, tail) = rest.split_at(1);
        // The operand runs to the next operator.
        let end = tail.find(['/', '+', '-']).unwrap_or(tail.len());
        let (operand, next) = tail.split_at(end);
        match op {
            "/" => now = round_down(now, parse_unit(operand).ok_or_else(&err)?).ok_or_else(&err)?,
            "+" | "-" => {
                let digits_end = operand
                    .find(|c: char| !c.is_ascii_digit())
                    .ok_or_else(&err)?;
                if digits_end == 0 {
                    return Err(err());
                }
                let (count, unit) = operand.split_at(digits_end);
                let count: i64 = count.parse().map_err(|_| err())?;
                let unit = parse_unit(unit).ok_or_else(&err)?;
                let signed = if op == "-" { -count } else { count };
                now = shift(now, unit, signed).ok_or_else(&err)?;
            }
            _ => return Err(err()),
        }
        rest = next;
    }
    to_millis(now).ok_or_else(err)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    Year,
    Month,
    Week,
    Day,
    Hour,
    Minute,
    Second,
    Milli,
}

fn parse_unit(raw: &str) -> Option<Unit> {
    Some(match raw {
        "YEAR" | "YEARS" => Unit::Year,
        "MONTH" | "MONTHS" => Unit::Month,
        "WEEK" | "WEEKS" => Unit::Week,
        "DAY" | "DAYS" | "DATE" => Unit::Day,
        "HOUR" | "HOURS" => Unit::Hour,
        "MINUTE" | "MINUTES" | "MIN" | "MINS" => Unit::Minute,
        "SECOND" | "SECONDS" | "SEC" | "SECS" => Unit::Second,
        "MILLI" | "MILLIS" | "MILLISECOND" | "MILLISECONDS" => Unit::Milli,
        _ => return None,
    })
}

fn round_down(dt: OffsetDateTime, unit: Unit) -> Option<OffsetDateTime> {
    let date = dt.date();
    let midnight = |d: Date| Some(PrimitiveDateTime::new(d, Time::MIDNIGHT).assume_utc());
    match unit {
        Unit::Year => midnight(Date::from_calendar_date(date.year(), Month::January, 1).ok()?),
        Unit::Month => midnight(Date::from_calendar_date(date.year(), date.month(), 1).ok()?),
        // Solr's `/WEEK` rounds to the start of the week; `time`'s ISO week
        // starts on Monday, which is what Solr's default locale uses too.
        Unit::Week => {
            let back = i64::from(date.weekday().number_days_from_monday());
            midnight(date.checked_sub(Duration::days(back))?)
        }
        Unit::Day => midnight(date),
        Unit::Hour => Some(dt.replace_time(Time::from_hms(dt.hour(), 0, 0).ok()?)),
        Unit::Minute => Some(dt.replace_time(Time::from_hms(dt.hour(), dt.minute(), 0).ok()?)),
        Unit::Second => {
            Some(dt.replace_time(Time::from_hms(dt.hour(), dt.minute(), dt.second()).ok()?))
        }
        Unit::Milli => Some(dt.replace_time(
            Time::from_hms_milli(dt.hour(), dt.minute(), dt.second(), dt.millisecond()).ok()?,
        )),
    }
}

fn shift(dt: OffsetDateTime, unit: Unit, count: i64) -> Option<OffsetDateTime> {
    match unit {
        Unit::Year => {
            let year = dt.year().checked_add(i32::try_from(count).ok()?)?;
            // A leap day shifted into a non-leap year clamps to the 28th, the
            // same way Java's calendar arithmetic does.
            let day = dt.day().min(days_in_month(year, dt.month()));
            let date = Date::from_calendar_date(year, dt.month(), day).ok()?;
            Some(PrimitiveDateTime::new(date, dt.time()).assume_utc())
        }
        Unit::Month => {
            let total = i64::from(dt.year()) * 12 + i64::from(u8::from(dt.month())) - 1 + count;
            let year = i32::try_from(total.div_euclid(12)).ok()?;
            let month = Month::try_from(u8::try_from(total.rem_euclid(12) + 1).ok()?).ok()?;
            let day = dt.day().min(days_in_month(year, month));
            let date = Date::from_calendar_date(year, month, day).ok()?;
            Some(PrimitiveDateTime::new(date, dt.time()).assume_utc())
        }
        Unit::Week => dt.checked_add(Duration::weeks(count)),
        Unit::Day => dt.checked_add(Duration::days(count)),
        Unit::Hour => dt.checked_add(Duration::hours(count)),
        Unit::Minute => dt.checked_add(Duration::minutes(count)),
        Unit::Second => dt.checked_add(Duration::seconds(count)),
        Unit::Milli => dt.checked_add(Duration::milliseconds(count)),
    }
}

fn days_in_month(year: i32, month: Month) -> u8 {
    tantivy::time::util::days_in_month(month, year)
}

/// Renders a millisecond timestamp as the RFC3339 string the dynamic path
/// carries inside the catch-all JSON object, where Tantivy re-detects it as a
/// date and gives it its own fast column.
pub fn millis_to_rfc3339(ms: i64) -> Option<String> {
    let dt = OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).ok()?;
    dt.format(&Rfc3339).ok()
}

/// Whether a document whose `date_range` field holds `members` (one
/// `(start_ms, end_ms)` pair per value, in index order) satisfies `op` against
/// the query interval `q` — finding 168's union-of-members set relations.
///
/// A document with no member matches nothing, which is what keeps
/// `drs_x:[* TO *]` to the docs that actually HAVE the field
/// (`dr341_star_both`: d1-d7, never d8/d9).
pub fn matches(op: Op, members: &[(i64, i64)], q: Interval) -> bool {
    if members.is_empty() {
        return false;
    }
    match op {
        // Hole-sensitive: at least one real member must overlap the query.
        // A min-start/max-end collapse would match `dr341_multi_gap`'s d8 on a
        // query lying entirely inside its 2021 hole.
        Op::Intersects => members
            .iter()
            .any(|(s, e)| *s <= q.end_ms && *e >= q.start_ms),
        // The union must fit inside the query, so EVERY member must
        // (`dr341_multi_within_one`: d8's 2022-05 member disqualifies it even
        // though its 2020 member fits). Equivalent to min(start)/max(end),
        // which is the reduction finding 168 states.
        Op::Within => members
            .iter()
            .all(|(s, e)| *s >= q.start_ms && *e <= q.end_ms),
        // Hole-sensitive in the other direction: the query must sit inside one
        // *contiguous* run of the union, so members are merged first (touching
        // or millisecond-adjacent members form one run) and the query must fit
        // inside a single merged run. Per-member alone would already satisfy
        // every current fixture; merging is the semantically correct reading of
        // "the union of its members" and costs one sort.
        Op::Contains => {
            let mut runs: Vec<(i64, i64)> = members.to_vec();
            runs.sort_unstable();
            let mut merged: Vec<(i64, i64)> = Vec::with_capacity(runs.len());
            for (s, e) in runs {
                match merged.last_mut() {
                    Some(last) if s <= last.1.saturating_add(1) => last.1 = last.1.max(e),
                    _ => merged.push((s, e)),
                }
            }
            merged
                .iter()
                .any(|(s, e)| *s <= q.start_ms && *e >= q.end_ms)
        }
    }
}

/// A `date_range` interval predicate as a Tantivy query: `AllQuery` narrowed
/// by [`matches`] over the two endpoint fast columns, read member by member so
/// the pairing between a multiValued field's starts and ends survives (finding
/// 168). Modelled on `crate::function_query::GeoFilterQuery`, which filters
/// `AllQuery` by two synthetic columns the same way.
///
/// The column names are resolved by
/// `WayfinderSchema::resolved_date_range_columns`, so the same query serves a
/// declared static field (`<name>__start`/`__end`) and a dynamic one
/// (`_dynamic.<name>.start`/`.end`) with no other difference.
pub struct DateRangeQuery {
    start_column: String,
    end_column: String,
    op: Op,
    interval: Interval,
}

impl DateRangeQuery {
    pub fn new(start_column: String, end_column: String, op: Op, interval: Interval) -> Self {
        DateRangeQuery {
            start_column,
            end_column,
            op,
            interval,
        }
    }
}

impl fmt::Debug for DateRangeQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DateRangeQuery")
            .field("start_column", &self.start_column)
            .field("end_column", &self.end_column)
            .field("op", &self.op)
            .field("interval", &self.interval)
            .finish()
    }
}

impl Clone for DateRangeQuery {
    fn clone(&self) -> Self {
        DateRangeQuery {
            start_column: self.start_column.clone(),
            end_column: self.end_column.clone(),
            op: self.op,
            interval: self.interval,
        }
    }
}

impl Query for DateRangeQuery {
    fn weight(&self, enable_scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        // Constant-score filter over every alive document, exactly like
        // `GeoFilterQuery`: `enable_scoring` is deliberately ignored.
        let child = tantivy::query::AllQuery.weight(enable_scoring)?;
        Ok(Box::new(DateRangeWeight {
            child,
            start_column: self.start_column.clone(),
            end_column: self.end_column.clone(),
            op: self.op,
            interval: self.interval,
        }))
    }

    fn query_terms<'a>(&'a self, _visitor: &mut dyn FnMut(&'a Term, bool)) {
        // Membership comes from the fast columns; there are no term-dictionary
        // clauses to report (same as `GeoFilterQuery`).
    }
}

struct DateRangeWeight {
    child: Box<dyn Weight>,
    start_column: String,
    end_column: String,
    op: Op,
    interval: Interval,
}

/// The two endpoint columns for one segment. Either being absent (no document
/// in this segment carries the field) makes every document a non-match.
struct EndpointColumns {
    start: Option<Column<DateTime>>,
    end: Option<Column<DateTime>>,
}

impl EndpointColumns {
    fn open(reader: &SegmentReader, start: &str, end: &str) -> tantivy::Result<EndpointColumns> {
        let fast = reader.fast_fields();
        Ok(EndpointColumns {
            start: fast.column_opt::<DateTime>(start)?,
            end: fast.column_opt::<DateTime>(end)?,
        })
    }

    /// This document's interval members, paired by ordinal.
    /// `Column::values_for_doc` yields a document's values in the order they
    /// were indexed (deliberately unsorted), so ordinal `i` of `.start` pairs
    /// with ordinal `i` of `.end` — the property the hole-sensitive predicates
    /// depend on. A ragged pair (one column short) simply drops the unpaired
    /// tail, which cannot happen through `add_values`/`coerce_json` but must
    /// not be a panic.
    fn members(&self, doc: DocId) -> Vec<(i64, i64)> {
        let (Some(start), Some(end)) = (&self.start, &self.end) else {
            return Vec::new();
        };
        start
            .values_for_doc(doc)
            .zip(end.values_for_doc(doc))
            .map(|(s, e)| (s.into_timestamp_millis(), e.into_timestamp_millis()))
            .collect()
    }
}

impl Weight for DateRangeWeight {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        let child = self.child.scorer(reader, boost)?;
        let columns = EndpointColumns::open(reader, &self.start_column, &self.end_column)?;
        let mut scorer = DateRangeScorer {
            child,
            columns,
            op: self.op,
            interval: self.interval,
        };
        // `DocSet` iteration is `doc()`-first, so a fresh scorer must already
        // sit on its first match (see `GeoFilterWeight::scorer`).
        scorer.position_at_first_match();
        Ok(Box::new(scorer))
    }

    fn explain(&self, reader: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        let columns = EndpointColumns::open(reader, &self.start_column, &self.end_column)?;
        let members = columns.members(doc);
        let hit = matches(self.op, &members, self.interval);
        let mut explanation = Explanation::new_with_string(
            format!("date_range {:?}", self.op),
            if hit { 1.0 } else { 0.0 },
        );
        explanation.add_detail(Explanation::new_with_string(
            format!("members={members:?} query={:?}", self.interval),
            if hit { 1.0 } else { 0.0 },
        ));
        Ok(explanation)
    }

    fn count(&self, reader: &SegmentReader) -> tantivy::Result<u32> {
        // The matched set is a subset of `AllQuery`'s, so scan for an exact
        // count (`GeoFilterWeight::count`'s reasoning).
        let mut scorer = self.scorer(reader, 1.0)?;
        let mut n = 0u32;
        while scorer.advance() != TERMINATED {
            n += 1;
        }
        Ok(n)
    }
}

struct DateRangeScorer {
    child: Box<dyn Scorer>,
    columns: EndpointColumns,
    op: Op,
    interval: Interval,
}

impl DateRangeScorer {
    fn matches_at(&self, doc: DocId) -> bool {
        matches(self.op, &self.columns.members(doc), self.interval)
    }

    fn position_at_first_match(&mut self) {
        while self.child.doc() != TERMINATED && !self.matches_at(self.child.doc()) {
            self.child.advance();
        }
    }
}

impl DocSet for DateRangeScorer {
    fn advance(&mut self) -> DocId {
        loop {
            let doc = self.child.advance();
            if doc == TERMINATED {
                return TERMINATED;
            }
            if self.matches_at(doc) {
                return doc;
            }
        }
    }

    fn doc(&self) -> DocId {
        self.child.doc()
    }

    fn size_hint(&self) -> u32 {
        self.child.size_hint()
    }
}

impl Scorer for DateRangeScorer {
    fn score(&mut self) -> Score {
        // Constant score, matching Solr's `ConstantScoreQuery` for a parsed
        // spatial predicate.
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(s: &str) -> i64 {
        to_millis(OffsetDateTime::parse(s, &Rfc3339).expect("rfc3339")).expect("in range")
    }

    #[test]
    fn bare_year_expands_to_the_whole_year_end_inclusive() {
        assert_eq!(
            parse_interval("2020").expect("parses"),
            Interval {
                start_ms: ms("2020-01-01T00:00:00Z"),
                end_ms: ms("2020-12-31T23:59:59.999Z"),
            }
        );
    }

    #[test]
    fn bare_month_expands_to_the_whole_month() {
        assert_eq!(
            parse_interval("2020-06").expect("parses"),
            Interval {
                start_ms: ms("2020-06-01T00:00:00Z"),
                end_ms: ms("2020-06-30T23:59:59.999Z"),
            }
        );
    }

    #[test]
    fn a_full_instant_literal_is_the_whole_stated_second() {
        assert_eq!(
            parse_interval("2019-12-31T23:59:59Z").expect("parses"),
            Interval {
                start_ms: ms("2019-12-31T23:59:59Z"),
                end_ms: ms("2019-12-31T23:59:59.999Z"),
            }
        );
    }

    #[test]
    fn interval_endpoints_expand_at_their_own_precision() {
        // Finding 166's interval-endpoint half: the end token is a month, so
        // the interval ends on the last millisecond of that month.
        assert_eq!(
            parse_interval("[2020-03 TO 2020-09]").expect("parses"),
            Interval {
                start_ms: ms("2020-03-01T00:00:00Z"),
                end_ms: ms("2020-09-30T23:59:59.999Z"),
            }
        );
    }

    #[test]
    fn brace_form_is_identical_to_the_bracket_form() {
        assert_eq!(
            parse_interval("{2020-05-01T00:00:00Z TO 2020-07-01T00:00:00Z}").expect("parses"),
            parse_interval("[2020-05-01T00:00:00Z TO 2020-07-01T00:00:00Z]").expect("parses"),
        );
    }

    #[test]
    fn star_endpoints_are_the_open_bounds() {
        assert_eq!(
            parse_interval("[* TO *]").expect("parses"),
            Interval {
                start_ms: MIN_MS,
                end_ms: MAX_MS
            }
        );
    }

    #[test]
    fn reversed_interval_is_unsupported_not_a_parse_error() {
        let err = parse_interval("[2021 TO 2020]").expect_err("reversed");
        assert!(matches!(err, DateRangeError::Unsupported(_)), "{err:?}");
        assert_eq!(err.to_string(), "Wrong order: 2021 TO 2020");
    }

    #[test]
    fn bad_month_is_a_parse_error() {
        let err = parse_interval("[2020-13 TO 2021]").expect_err("bad month");
        assert!(matches!(err, DateRangeError::Parse(_)), "{err:?}");
        assert_eq!(
            err.to_string(),
            "Couldn't parse date because: Improperly formatted datetime: 2020-13"
        );
    }

    #[test]
    fn bad_date_math_is_a_parse_error() {
        let err = parse_interval("[NOW/BOGUS TO NOW]").expect_err("bad math");
        assert!(matches!(err, DateRangeError::Parse(_)), "{err:?}");
        assert_eq!(err.to_string(), "Invalid Date Math String:'NOW/BOGUS'");
    }

    #[test]
    fn date_math_rounds_and_shifts() {
        let year = parse_interval("[NOW/YEAR TO NOW/YEAR+1YEAR]").expect("parses");
        let now = OffsetDateTime::now_utc();
        assert_eq!(
            year.start_ms,
            ms(&format!("{}-01-01T00:00:00Z", now.year()))
        );
        assert_eq!(
            year.end_ms,
            ms(&format!("{}-01-01T00:00:00Z", now.year() + 1))
        );
    }

    #[test]
    fn op_aliases_and_case_insensitivity() {
        assert_eq!(parse_op("Intersects").expect("op"), Op::Intersects);
        assert_eq!(parse_op("contains").expect("op"), Op::Contains);
        assert_eq!(parse_op("IsWithin").expect("op"), Op::Within);
        assert_eq!(parse_op("WITHIN").expect("op"), Op::Within);
        assert_eq!(
            parse_op("IsDisjointTo")
                .expect_err("unimplemented")
                .to_string(),
            "Disjoint"
        );
        assert_eq!(
            parse_op("Bogus").expect_err("unknown").to_string(),
            "Unknown Operation: Bogus"
        );
    }

    #[test]
    fn multivalued_predicates_are_hole_sensitive() {
        // d8: `["2020", "2022-05"]` -- a hole covering 2021.
        let d8 = [
            (ms("2020-01-01T00:00:00Z"), ms("2020-12-31T23:59:59.999Z")),
            (ms("2022-05-01T00:00:00Z"), ms("2022-05-31T23:59:59.999Z")),
        ];
        let hole = Interval {
            start_ms: ms("2021-01-01T00:00:00Z"),
            end_ms: ms("2021-06-30T23:59:59.999Z"),
        };
        assert!(!matches(Op::Intersects, &d8, hole));
        assert!(!matches(Op::Contains, &d8, hole));
        let spanning = Interval {
            start_ms: ms("2020-06-01T00:00:00Z"),
            end_ms: ms("2022-01-31T23:59:59.999Z"),
        };
        assert!(!matches(Op::Contains, &d8, spanning));
        let within_2020 = Interval {
            start_ms: ms("2020-01-01T00:00:00Z"),
            end_ms: ms("2020-12-31T23:59:59.999Z"),
        };
        assert!(!matches(Op::Within, &d8, within_2020));
        assert!(matches(Op::Within, &d8[..1], within_2020));
    }

    #[test]
    fn a_field_with_no_member_matches_nothing() {
        let open = Interval {
            start_ms: MIN_MS,
            end_ms: MAX_MS,
        };
        assert!(!matches(Op::Intersects, &[], open));
        assert!(!matches(Op::Within, &[], open));
        assert!(!matches(Op::Contains, &[], open));
    }
}
