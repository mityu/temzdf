// URL and URLSearchParams from core-js (its standalone build, which
// leaves the globals alone: they are set here, when missing).
import URL from "core-js-pure/stable/url/index.js";
import URLSearchParams from "core-js-pure/stable/url-search-params/index.js";

const g = globalThis as Record<string, unknown>;
g.URL ??= URL;
g.URLSearchParams ??= URLSearchParams;
