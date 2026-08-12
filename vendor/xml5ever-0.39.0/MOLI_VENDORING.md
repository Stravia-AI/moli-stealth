# xml5ever 0.39.0

This directory contains the source of the crates.io `xml5ever` 0.39.0 release.
It is patched locally because Moli needs the tokenizer to preserve CDATA as a
distinct token, the XML tree builder to surface CDATA nodes to its sink, and
namespace declarations to remain available as DOM attributes after binding.

The upstream source is licensed under MIT or Apache-2.0; see `LICENSE-MIT` and
`LICENSE-APACHE` in this directory.
