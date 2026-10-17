# Vendored front-end dependencies

Pinned copies of the four libraries `src/index.html` used to load from
cdn.jsdelivr.net (ITER-063, research F19). They ship inside the app bundle so
the GUI and the headless `render`/`batch` CLI start with no network and no
third-party fetch.

| Package | File | Upstream URL (byte-identical) | License |
|---|---|---|---|
| p5 1.11.2 | `p5-1.11.2/p5.min.js` | https://cdn.jsdelivr.net/npm/p5@1.11.2/lib/p5.min.js | LGPL-2.1 |
| JSZip 3.10.1 | `jszip-3.10.1/jszip.min.js` | https://cdn.jsdelivr.net/npm/jszip@3.10.1/dist/jszip.min.js | MIT or GPL-3.0 (dual) |
| fflate 0.8.2 | `fflate-0.8.2/index.js` | https://cdn.jsdelivr.net/npm/fflate@0.8.2/umd/index.js | MIT |
| Tweakpane 3.1.10 | `tweakpane-3.1.10/tweakpane.min.js` | https://cdn.jsdelivr.net/npm/tweakpane@3.1.10/dist/tweakpane.min.js | MIT |

`SHA256SUMS` pins every file; `tests/frontend-contracts.test.js` re-hashes
them on each `npm test`. To verify against the CDN by hand:

    curl -sL <upstream URL> | shasum -a 256

Provenance: the bytes are the CDN responses for the URLs above, taken from the
app's own WebKit network cache and cross-checked (sha256) against a second,
independent copy on disk. LICENSE files: JSZip's is the package's own
`LICENSE.markdown`; fflate's is the package's MIT `LICENSE`; p5's is the
canonical FSF LGPL-2.1 text (p5's `license.txt`); Tweakpane's is the MIT text
with the copyright line from the file's own banner.

To upgrade: replace the file, update the directory name, `SHA256SUMS`, the
`<script>` tag in `src/index.html`, and this table in one commit.
