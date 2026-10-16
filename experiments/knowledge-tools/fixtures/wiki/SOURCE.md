# Source and licence

Each JSON file here is the plain-text extract of one English Wikipedia article, fetched on
2026-10-14 by `../../fetch_fixtures.py` through the MediaWiki API (`prop=extracts`,
`explaintext=1`). Its `url` field is the article it came from.

The text is Wikipedia's, by its contributors, under the
[Creative Commons Attribution-ShareAlike 4.0](https://creativecommons.org/licenses/by-sa/4.0/)
licence. It is kept unmodified, as frozen test input, so that bench runs compare tool designs
and not the network.
