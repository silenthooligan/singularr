#!/usr/bin/env python3
"""Checks that no common content blocker blocks Singularr's own requests.

A browser with uBlock Origin, AdGuard or Brave's shields applies tracker and ad lists to every request a page makes,
first-party ones included. A rule meant for some site's tracker can match one of Singularr's own addresses: EasyPrivacy
holds `/api/v1/activity|$~third-party,xmlhttprequest`, which blocked the Activity page's record in every such
browser. This tool fetches the lists those blockers turn on by default and tests every address the web client asks
for, as a first-party request of its real type:

  - the API paths the client names in its code, and every route the server documents (app/src; with --openapi, a
    live document's too),
  - the client's own files (scripts, styles, fonts, images, pages).

It prints each address a rule blocks, with the rule and its list, and exits 1 when there is one. Standard library only.

Usage: python3 tools/blockcheck.py [--openapi <file or URL>] [--lists <dir to keep the lists>] [--offline]
  --offline  use the lists already in --lists (no download); a missing list is an error
"""
import argparse, json, os, re, sys, urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
WEB = os.path.join(ROOT, "web")
HOST = "singularr.example.org"

# The lists a fresh install of each blocker turns on (uBlock Origin's defaults, AdGuard's base and tracking lists,
# which Brave's shields share with EasyList and EasyPrivacy), plus uBlock's own exceptions.
LISTS = {
    "EasyList": "https://easylist.to/easylist/easylist.txt",
    "EasyPrivacy": "https://easylist.to/easylist/easyprivacy.txt",
    "uBlock filters": "https://ublockorigin.github.io/uAssets/filters/filters.min.txt",
    "uBlock privacy": "https://ublockorigin.github.io/uAssets/filters/privacy.min.txt",
    "uBlock badware": "https://ublockorigin.github.io/uAssets/filters/badware.min.txt",
    "uBlock quick fixes": "https://ublockorigin.github.io/uAssets/filters/quick-fixes.min.txt",
    "uBlock unbreak": "https://ublockorigin.github.io/uAssets/filters/unbreak.min.txt",
    "AdGuard Base": "https://filters.adtidy.org/extension/ublock/filters/2.txt",
    "AdGuard Tracking Protection": "https://filters.adtidy.org/extension/ublock/filters/3.txt",
}

TYPES = {
    "script": "script", "stylesheet": "stylesheet", "css": "stylesheet", "image": "image", "img": "image",
    "xmlhttprequest": "xhr", "xhr": "xhr", "font": "font", "media": "media", "object": "object",
    "subdocument": "frame", "frame": "frame", "ping": "ping", "beacon": "ping", "websocket": "websocket",
    "document": "document", "doc": "document", "other": "other", "webrtc": "webrtc", "popup": "popup",
}
# Options that change a response or the page instead of blocking the request.
NOT_BLOCKING = {"removeparam", "csp", "redirect-rule", "header", "permissions", "urltransform", "uritransform",
                "replace", "cookie", "stealth", "jsonprune", "hls", "xmlprune", "urlskip", "responseheader",
                "elemhide", "ehide", "generichide", "ghide", "specifichide", "shide", "genericblock", "inline-script",
                "inline-font", "popunder", "content", "jsinject", "extension", "network", "app", "referrerpolicy"}
FIRST_PARTY_ONLY = {"~third-party", "first-party", "1p", "~3p", "strict1p", "strict-first-party"}
THIRD_PARTY_ONLY = {"third-party", "3p", "~first-party", "~1p", "strict3p", "strict-third-party"}


class Rule:
    __slots__ = ("text", "source", "exception", "important", "source_re", "flags", "compiled", "literal", "types",
                 "not_types", "domains", "not_domains", "methods", "document_ok")

    @property
    def regex(self):
        # Compiled on first use: most rules are dropped by the text check before they are ever tried.
        if self.compiled is None:
            try:
                self.compiled = re.compile(self.source_re, self.flags)
            except re.error:
                self.compiled = re.compile(r"(?!)")
        return self.compiled


def to_regex(pattern, match_case):
    """The rule's pattern as regular expression source, its flags, and the longest fixed text any match holds."""
    flags = 0 if match_case else re.I
    if len(pattern) > 2 and pattern.startswith("/") and pattern.endswith("/"):
        return pattern[1:-1], flags, ""
    start, end = "", ""
    if pattern.startswith("||"):
        start, pattern = r"^[a-z][a-z0-9+.-]*://(?:[^/?#]*\.)?", pattern[2:]
    elif pattern.startswith("|"):
        start, pattern = "^", pattern[1:]
    if pattern.endswith("|"):
        end, pattern = "$", pattern[:-1]
    out = []
    for ch in pattern:
        out.append(".*" if ch == "*" else r"(?:[^\w\-.%]|$)" if ch == "^" else re.escape(ch))
    literal = max(re.split(r"[*^|]", pattern), key=len).lower()
    return start + "".join(out) + end, flags, literal


def parse(line, source):
    line = line.strip()
    if not line or line[0] in "![" or line.startswith("#"):
        return None
    if re.search(r"#[@?$%]*#|\$\$|\$@\$", line):
        return None  # cosmetic, scriptlet or HTML filter
    rule = Rule()
    rule.text, rule.source = line, source
    rule.exception = line.startswith("@@")
    if rule.exception:
        line = line[2:]
    pattern, opts = line, []
    m = re.match(r"^(/.*/)(?:\$(.*))?$", line) if line.startswith("/") and "/" in line[1:] else None
    if m and not re.match(r"^/[\w./-]*$", m.group(1).rstrip("/")):
        pattern, opts = m.group(1), (m.group(2) or "").split(",") if m.group(2) else []
    elif "$" in line:
        i = line.rfind("$")
        pattern, opts = line[:i], line[i + 1:].split(",")
    rule.important, rule.types, rule.not_types = False, set(), set()
    rule.domains, rule.not_domains, rule.methods, rule.document_ok = [], [], set(), False
    match_case = False
    for o in (x.strip() for x in opts if x.strip()):
        name, _, value = o.partition("=")
        bare = name.lstrip("~")
        if name in THIRD_PARTY_ONLY or bare in ("badfilter", "popup", "to", "denyallow", "ipaddress") or bare in NOT_BLOCKING:
            return None
        if name in FIRST_PARTY_ONLY or bare in ("redirect", "rewrite", "empty", "mp4", "all"):
            if bare == "all":
                rule.document_ok = True
            continue
        if bare == "important":
            rule.important = True
        elif bare == "match-case":
            match_case = True
        elif bare in ("domain", "from"):
            for d in value.split("|"):
                (rule.not_domains if d.startswith("~") else rule.domains).append(d.lstrip("~").lower())
        elif bare == "method":
            rule.methods = {x.lstrip("~").upper() for x in value.split("|") if not x.startswith("~")}
        elif bare in TYPES:
            t = TYPES[bare]
            (rule.not_types if name.startswith("~") else rule.types).add(t)
            if t == "document" and not name.startswith("~"):
                rule.document_ok = True
        # any other option (sitekey, rewrite targets, priorities) leaves the rule as it is
    rule.source_re, rule.flags, rule.literal = to_regex(pattern or "*", match_case)
    rule.compiled = None
    # A plain host rule (||host^) blocks a whole page too, in uBlock's strict blocking.
    if re.fullmatch(r"\|\|[\w.-]+\^?", pattern):
        rule.document_ok = True
    return rule


def applies(rule, url, low, kind, method):
    if rule.literal and rule.literal not in low:
        return False
    if rule.domains and not any(HOST == d or HOST.endswith("." + d) for d in rule.domains):
        return False
    if any(HOST == d or HOST.endswith("." + d) for d in rule.not_domains):
        return False
    if rule.methods and method not in rule.methods:
        return False
    if kind == "document":
        if not rule.document_ok:
            return False
    elif rule.types and kind not in rule.types:
        return False
    if kind in rule.not_types:
        return False
    return bool(rule.regex.search(url))


def load_lists(keep, offline):
    rules = []
    os.makedirs(keep, exist_ok=True)
    for name, url in LISTS.items():
        path = os.path.join(keep, re.sub(r"\W+", "-", name.lower()) + ".txt")
        if not offline:
            req = urllib.request.Request(url, headers={"User-Agent": "singularr-blockcheck"})
            with urllib.request.urlopen(req, timeout=60) as r, open(path, "wb") as f:
                f.write(r.read())
        with open(path, encoding="utf-8", errors="replace") as f:
            for line in f:
                r = parse(line, name)
                if r:
                    rules.append(r)
    return rules


def client_files():
    """The client's own files as the browser asks for them: (path, type)."""
    kinds = {".js": "script", ".mjs": "script", ".css": "stylesheet", ".woff2": "font", ".woff": "font", ".ttf": "font",
             ".svg": "image", ".png": "image", ".jpg": "image", ".jpeg": "image", ".webp": "image", ".ico": "image",
             ".avif": "image", ".gif": "image", ".json": "xhr", ".webmanifest": "other", ".html": "document"}
    out = []
    for dirpath, dirnames, files in os.walk(WEB):
        rel_dir = os.path.relpath(dirpath, WEB).replace("\\", "/")
        if rel_dir.split("/")[0] in ("data", "tools"):
            continue  # the demo's sample set and the design tools are not served by the product
        for f in files:
            ext = os.path.splitext(f)[1].lower()
            if ext not in kinds:
                continue
            rel = ("" if rel_dir == "." else rel_dir + "/") + f
            out.append(("/" + rel, kinds[ext]))
            if ext == ".html":
                out.append(("/" + rel[:-5], "document"))  # the clean address the server also answers
    return out


SAMPLE = {"id": "1234", "kind": "movie", "name": "general", "tab": "queue", "key": "abc", "slug": "movies",
          "library": "1", "section": "1", "path": "x", "type": "radarr", "file": "1", "season": "1", "episode": "1"}


def fill(path):
    return re.sub(r"\{([^}]+)\}", lambda m: SAMPLE.get(m.group(1).lower(), "1"), path)


def client_api():
    """The API addresses the client's code names, with any ${...} part filled: (path, method)."""
    found = set()
    for dirpath, _, files in os.walk(os.path.join(WEB, "js")):
        for f in files:
            if not f.endswith(".js"):
                continue
            src = open(os.path.join(dirpath, f), encoding="utf-8").read()
            for m in re.finditer(r"""MQ\.send\(\s*['"`](GET|POST|PUT|PATCH|DELETE)['"`]\s*,\s*['"`](/[^'"`]*)""", src):
                found.add((m.group(2), m.group(1)))
            for m in re.finditer(r"""(?:MQ\.api|MQ\.fetchJson|api|lean\([^,]+,)\s*\(?\s*['"`](/[a-z][^'"`]*)""", src):
                found.add((m.group(1), "GET"))
            for m in re.finditer(r"""['"`](/api/v1/[^'"`\s]*)""", src):
                found.add((m.group(1)[len("/api/v1"):], "GET"))
            for m in re.finditer(r"""\[/\^[^\]]*?,\s*['"`](/[a-z][^'"`]*)['"`]""", src):
                found.add((m.group(1), "GET"))
    out = []
    for path, method in found:
        path = re.sub(r"\$\{[^}]*\}", "1", path)
        out.append(("/api/v1" + path if not path.startswith("/api/") else path, method))
    return out


def rust_paths():
    """The routes the server documents (the utoipa path attributes in app/src): (path, method)."""
    out = []
    for dirpath, _, files in os.walk(os.path.join(ROOT, "app", "src")):
        for f in files:
            if f.endswith(".rs"):
                src = open(os.path.join(dirpath, f), encoding="utf-8").read()
                for m in re.finditer(r'utoipa::path\(\s*(get|post|put|patch|delete)\s*,\s*path\s*=\s*"([^"]+)"', src):
                    out.append((fill(m.group(2)), m.group(1).upper()))
    return out


def openapi_paths(src):
    if re.match(r"^https?://", src):
        with urllib.request.urlopen(src, timeout=30) as r:
            doc = json.load(r)
    else:
        doc = json.load(open(src, encoding="utf-8"))
    out = []
    for path, ops in (doc.get("paths") or {}).items():
        for method in ops:
            if method.upper() in ("GET", "POST", "PUT", "PATCH", "DELETE"):
                out.append((fill(path), method.upper()))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--openapi")
    ap.add_argument("--lists", default=os.path.join(ROOT, "target", "blocklists"))
    ap.add_argument("--offline", action="store_true")
    a = ap.parse_args()
    rules = load_lists(a.lists, a.offline)
    blocks = [r for r in rules if not r.exception]
    allows = [r for r in rules if r.exception]
    tests = [(p, "xhr", m) for p, m in client_api()]
    tests += [(p, "xhr", m) for p, m in rust_paths()]
    if a.openapi:
        tests += [(p, "xhr", m) for p, m in openapi_paths(a.openapi)]
    tests += [(p, k, "GET") for p, k in client_files()]
    # Each API address is also asked with a query, the way pages ask for parts and pages of a record.
    tests += [(p + "?limit=50", k, m) for p, k, m in tests if p.startswith("/api/") and "?" not in p]
    tests = list(dict.fromkeys(tests))
    # A rule whose fixed text appears in none of the addresses cannot match one: most rules name other sites, so one
    # pass over all the addresses at once leaves a few thousand to try address by address.
    corpus = "\n".join(f"https://{HOST}{p}".lower() for p, _, _ in tests)
    blocks = [r for r in blocks if not r.literal or r.literal in corpus]
    allows = [r for r in allows if not r.literal or r.literal in corpus]
    seen, hits = set(), []
    for path, kind, method in tests:
        seen.add((path, kind, method))
        url = f"https://{HOST}{path}"
        low = url.lower()
        block = next((r for r in blocks if applies(r, url, low, kind, method)), None)
        if not block:
            continue
        allow = next((r for r in allows if applies(r, url, low, kind, method)), None)
        if allow and not block.important:
            continue
        hits.append((path, kind, method, block))
    print(f"{len(rules)} rules from {len(LISTS)} lists; {len(seen)} addresses tested")
    for path, kind, method, r in sorted(hits, key=lambda h: h[0]):
        print(f"BLOCKED {method} {path} ({kind}) by {r.source}: {r.text}")
    return 1 if hits else 0


if __name__ == "__main__":
    sys.exit(main())
