# Extraction boundary

`POST /wayfinder/{core}/update/extract` accepts multipart uploads within the
limits in [server configuration](configuration.md). It can return extracted
content (`extractOnly=true`) or index supported content. See the canonical
[Compatibility](../../docs/COMPATIBILITY.md) and
[Configuration](../../docs/CONFIGURATION.md) documents for the wire and limits.

Supported dispatch is bounded: plain text, HTML, recognized OOXML/ODF containers,
RTF, and PDF have dedicated extractors. ZIP detection is refined only for
recognized office packages. **Generic XML dispatch is unsupported**: declaring
or naming XML does not make arbitrary XML an extractable document format.
Legacy OLE and arbitrary ZIP containers are unsupported. No OCR or external
extraction service is used.

`literal.<field>` and `fmap.<from>` are prefix families, not literal parameter
names; see [route parameter allowlists](parameters.md). They are not generic XML
mapping or XPath support.
