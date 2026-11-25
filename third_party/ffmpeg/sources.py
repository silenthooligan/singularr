#!/usr/bin/env python3
"""MANIFEST.txt and ffmpeg-NOTICES.txt of the corresponding source of the images' ffmpeg and ffprobe.

  python3 -I third_party/ffmpeg/sources.py <FFmpeg-Builds checkout> <enabled stages> <builds> <out>

<enabled stages>: the stage scripts the lgpl variant enables on either target, one path per line
(scripts.d/...), as .github/workflows/ffmpeg-source.yml asks each script's own ffbuild_enabled.
<builds>: the build's release archives, ffmpeg-<version>-<target>-lgpl-<addin>.tar.xz. <out>: each
stage's source as <stage>.tar.xz (archive_name) with its download commands beside it
(<stage>.tar.xz.dl), and the archives of FFmpeg (ffmpeg-<version>.tar.xz) and of FFmpeg-Builds
(FFmpeg-Builds-<commit>.tar.xz). The environment names their commits: FFMPEG_COMMIT and
FFBUILDS_COMMIT.

Writes <out>/MANIFEST.txt: every archive with its origin, revision, SHA-256 and the license its
license files name. And <out>/ffmpeg-NOTICES.txt: the license files of every library the builds
link. Those are the stages whose configure flag a build's configuration holds and the stages they
depend on. Each is written with the license files at the root of its source, verbatim, then those
of every crate it vendors. A text that repeats an earlier one is named and not written again.
third_party/ffmpeg/NOTICES.txt in the repository is a copy of ffmpeg-NOTICES.txt.

Nothing is run: the scripts are read as text, the archives in memory.
"""
import hashlib, os, re, sys, tarfile

NAMES = re.compile(r"^(LICEN[CS]E|COPYING|COPYRIGHT|NOTICE|UNLICENSE|PATENTS)([._-].*)?$", re.I)
# The first words that name a license, in the order they are tried.
KINDS = [
    (r"GNU LESSER GENERAL PUBLIC LICENSE\s+Version 3", "LGPL-3.0"),
    (r"GNU LESSER GENERAL PUBLIC LICENSE\s+Version 2\.1", "LGPL-2.1"),
    (r"GNU LIBRARY GENERAL PUBLIC LICENSE\s+Version 2", "LGPL-2.0"),
    (r"GNU GENERAL PUBLIC LICENSE\s+Version 3", "GPL-3.0"),
    (r"GNU GENERAL PUBLIC LICENSE\s+Version 2", "GPL-2.0"),
    (r"Mozilla Public License,?\s+(v\.|Version)\s*2\.0", "MPL-2.0"),
    (r"Apache License\s+Version 2\.0", "Apache-2.0"),
    (r"Boost Software License", "BSL-1.0"),
    (r"DO WHAT THE FUCK YOU WANT", "WTFPL"),
    (r"This is free and unencumbered software released into the public domain", "Unlicense"),
    (r"Permission to use, copy, modify, and/or distribute this software for any", "ISC or 0BSD"),
    (r"Permission is hereby granted, free of charge", "MIT"),
    (r"Redistribution and use in source and binary forms", "BSD"),
    (r"This software is provided 'as-is', without any express or implied", "Zlib"),
    (r"COPYRIGHT NOTICE, DISCLAIMER, and LICENSE", "Libpng"),
    (r"The FreeType Project LICENSE", "FTL"),
    (r"PATENT", "patent grant"),
]


def kind(text):
    for pattern, name in KINDS:
        if re.search(pattern, text[:4000], re.I):
            return name
    return "see the text"


def scripts_of(checkout):
    """Each stage script's repositories with their revisions, its configure flags and depends."""
    root = os.path.join(checkout, "scripts.d")
    out = {}
    for dirpath, dirs, files in os.walk(root):
        for f in sorted(files):
            if not f.endswith(".sh"):
                continue
            rel = "scripts.d/" + os.path.relpath(os.path.join(dirpath, f), root).replace("\\", "/")
            text = open(os.path.join(dirpath, f), encoding="utf-8", errors="replace").read()
            # The repository a script names, else its mirror (the one its commands fetch from).
            repos = dict(re.findall(r'^SCRIPT_MIRROR(\d*)="?([^"\n]+)"?', text, re.M))
            repos.update(re.findall(r'^SCRIPT_REPO(\d*)="?([^"\n]+)"?', text, re.M))
            revs = dict(re.findall(r'^SCRIPT_(?:COMMIT|TAG|REV)(\d*)="?([^"\n]+)"?', text, re.M))
            conf = re.search(r"ffbuild_configure\(\)\s*\{(.*?)\n\}", text, re.S)
            deps = re.search(r"ffbuild_depends\(\)\s*\{(.*?)\n\}", text, re.S)
            out[rel] = {
                "sources": [(repos[k], revs.get(k, "")) for k in sorted(repos)],
                "configure": re.findall(r"--enable-[\w-]+", conf.group(1) if conf else ""),
                "depends": [l.strip()[5:].strip() for l in (deps.group(1) if deps else "echo base").splitlines()
                            if l.strip().startswith("echo ")],
            }
    return out


def build_flags(builds):
    """The configure flags of every build archive: the configuration string in its ffprobe."""
    flags = set()
    for name in sorted(os.listdir(builds)):
        if not name.endswith(".tar.xz"):
            continue
        with tarfile.open(os.path.join(builds, name), "r:xz") as tf:
            for m in tf:
                if m.isfile() and m.name.endswith("/bin/ffprobe"):
                    data = tf.extractfile(m).read()
                    best = max((s for s in re.findall(rb"[ -~]{200,}", data) if b"--enable-" in s and b"--prefix=" in s),
                               key=len, default=b"")
                    flags.update(best.decode().split())
                    break
    return flags


def linked(scripts, enabled, flags):
    def stage(name):
        return [rel for rel in scripts
                if re.sub(r"^\d\d-", "", rel.split("/")[1]).removesuffix(".sh") == name]

    found, queue = set(), [r for r in enabled if any(f in flags for f in scripts[r]["configure"])]
    while queue:
        rel = queue.pop()
        if rel in found or rel not in enabled:
            continue
        found.add(rel)
        parts = rel.split("/")
        if len(parts) == 3:  # a stage in a folder is built with the folder
            queue.extend(s for s in scripts if s.split("/")[:2] == parts[:2])
        for dep in scripts[rel]["depends"]:
            queue.extend(stage(dep))
    return sorted(found)


def license_files(path):
    """The license files at the root of an archive, and those at the root of each vendored crate."""
    root, crates = {}, {}
    with tarfile.open(path, "r:xz") as tf:
        for m in tf:
            if not m.isfile():
                continue
            parts = [p for p in m.name.split("/") if p not in ("", ".")]
            if len(parts) == 1 and NAMES.match(parts[0]):
                root[parts[0]] = tf.extractfile(m).read().decode("utf-8", errors="replace")
            elif len(parts) == 3 and parts[0] == "vendor" and NAMES.match(parts[2]):
                crates.setdefault(parts[1], {})[parts[2]] = tf.extractfile(m).read().decode("utf-8", errors="replace")
    return root, crates


def archive_name(rel):
    """A stage's archive: its path under scripts.d, the folder joined to the name with `-`."""
    return rel.removeprefix("scripts.d/").removesuffix(".sh").replace("/", "-") + ".tar.xz"


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def main():
    checkout, enabled_file, builds, out = sys.argv[1:5]
    scripts = scripts_of(checkout)
    enabled = [l.strip() for l in open(enabled_file, encoding="utf-8") if l.strip()]
    flags = build_flags(builds)
    linked_stages = linked(scripts, set(enabled), flags)

    archives = {}
    for rel in enabled:
        path = os.path.join(out, archive_name(rel))
        if os.path.isfile(path):
            archives[rel] = path
    extra = sorted(n for n in os.listdir(out) if n.endswith(".tar.xz") and (n.startswith("ffmpeg-n") or n.startswith("FFmpeg-Builds-")))

    manifest = [
        "Corresponding source of the images' ffmpeg and ffprobe: FFmpeg-Builds' lgpl build for linux64 and linuxarm64.",
        "One block per archive: the component, where its source comes from, the revision, the archive's",
        "SHA-256, the license its license files name, and the commands that fetched it.",
    ]
    origins = {
        "ffmpeg-n": ("https://github.com/FFmpeg/FFmpeg.git", os.environ.get("FFMPEG_COMMIT", "")),
        "FFmpeg-Builds-": ("https://github.com/FFmpeg-Builds/FFmpeg-Builds.git", os.environ.get("FFBUILDS_COMMIT", "")),
    }
    for name in extra:
        root, _ = license_files(os.path.join(out, name))
        repo, rev = next(v for k, v in origins.items() if name.startswith(k))
        manifest += ["", name.removesuffix(".tar.xz"), f"  origin: {repo} at {rev}",
                     f"  archive: {name}, sha256 {sha256(os.path.join(out, name))}",
                     "  license: " + ("; ".join(f"{f} {kind(t)}" for f, t in sorted(root.items())) or "no license file at its root")]
    for rel in enabled:
        if rel not in archives:
            continue
        path = archives[rel]
        root, crates = license_files(path)
        dl = path + ".dl"
        commands = open(dl, encoding="utf-8").read().strip().splitlines() if os.path.isfile(dl) else []
        manifest += ["", rel.removesuffix(".sh").removeprefix("scripts.d/")]
        for repo, rev in scripts[rel]["sources"]:
            manifest.append(f"  origin: {repo} at {rev or 'the revision its commands name'}")
        manifest += [f"  archive: {os.path.basename(path)}, sha256 {sha256(path)}",
                     "  license: " + ("; ".join(f"{f} {kind(t)}" for f, t in sorted(root.items())) or "no license file at its root"),
                     f"  linked into the build: {'yes' if rel in linked_stages else 'no'}"]
        if crates:
            manifest.append(f"  vendored crates: {len(crates)}")
        manifest += ["  fetched by:"] + [f"    {c}" for c in commands]
    open(os.path.join(out, "MANIFEST.txt"), "w", encoding="utf-8", newline="\n").write("\n".join(manifest) + "\n")

    seen = {}
    notices = [
        "Notices of the libraries in ffmpeg and ffprobe",
        "==============================================",
        "",
        "The images' ffmpeg and ffprobe are FFmpeg-Builds' LGPL static build. Each library they link is",
        "listed with the license files at the root of its source, verbatim, as the corresponding source",
        "release holds it (MANIFEST.txt there names each revision). The crates a library vendors follow",
        "it. A text that repeats an earlier one is named and not written again. Code a library carries in",
        "its own tree has its notice in that tree. third_party/ffmpeg/sources.py wrote this file.",
    ]

    def put(label, name, text):
        digest = hashlib.sha256(text.strip().encode()).hexdigest()
        if digest in seen:
            notices.append(f"-- {name}: the same text as {seen[digest]} --")
            return
        seen[digest] = f"{label}, {name}"
        notices.append(f"-- {name} --")
        notices.extend(text.rstrip().splitlines())

    for n, rel in enumerate(linked_stages, 1):
        label = rel.removesuffix(".sh").removeprefix("scripts.d/")
        notices += ["", f"{n}. {label}"]
        for repo, rev in scripts[rel]["sources"]:
            notices.append(f"   {repo} at {rev or 'the revision its commands name'}")
        if rel not in archives:
            notices.append("-- no source archive --")
            continue
        root, crates = license_files(archives[rel])
        if not root:
            notices.append("-- no license file at the root of its source --")
        for name in sorted(root):
            put(label, name, root[name])
        for crate in sorted(crates):
            notices.append(f"   crate {crate}")
            for name in sorted(crates[crate]):
                put(f"{label}, crate {crate}", name, crates[crate][name])
    open(os.path.join(out, "ffmpeg-NOTICES.txt"), "w", encoding="utf-8", newline="\n").write("\n".join(notices) + "\n")
    print(f"{len(archives)} stage archives, {len(linked_stages)} linked, {len(seen)} distinct license texts")


if __name__ == "__main__":
    main()
