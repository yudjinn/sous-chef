"""Runs the verified anonymous python functions of src/eval/anonmodel/ with BitBake's own
datastore and writes the values tests/eval_anon_models.rs compares with (expected.json).

usage: oracle.py <live root> <release> <cases.json> <out.json>

<live root> is the live environment of docs/live-environment.md; <release> one of wrynose,
scarthgap, kirkstone: the cases of that release run with that release's BitBake
(`lib/bb`: `bb.data_smart.DataSmart`, `bb.data.inherits_class`, `bb.utils`, `bb.parse`) and
OE-core (`meta/lib`: `oe.utils`, `oe.license`, ...). Run it once per release; each run
updates the entries of its own cases in <out.json>.

For each case: a new DataSmart; `setVar` / `setVarFlag` of the case's variables, flags and
oracle-only variables; `__inherit_cache` empty. The function's text (`python () {` ...
`}`) becomes `def __anon(d):` with its body, compiled with the globals BitBake gives
anonymous python (`bb`, `os`, `oe` and the `oe.*` modules OE-core's `OE_IMPORTS` lists),
after the `def` functions the case names (from src/eval/anonmodel/) and, for base.bbclass,
the `def`s its anonymous python calls (from the release's base.bbclass and, on kirkstone,
license.bbclass). Then `__anon(d)` runs and each output is read with `getVar` (or
`getVarFlag` for `NAME[flag]`), expanded; `null` when unset.
"""
import json
import os
import re
import sys

live, release, cases_path, out_path = sys.argv[1:5]
here = os.path.dirname(os.path.abspath(__file__))
src = os.path.join(here, "..", "..", "..", "src", "eval", "anonmodel")

roots = {
    "wrynose": (os.path.join(live, "yocto-kiss", "bitbake"),
                os.path.join(live, "yocto-kiss", "openembedded-core", "meta"), "classes-global"),
    "scarthgap": (os.path.join(live, "releases", "scarthgap", "bitbake"),
                  os.path.join(live, "releases", "scarthgap", "openembedded-core", "meta"),
                  "classes-global"),
    "kirkstone": (os.path.join(live, "releases", "kirkstone", "bitbake"),
                  os.path.join(live, "releases", "kirkstone", "openembedded-core", "meta"),
                  "classes"),
}
bblib, meta, gclasses = roots[release]
sys.path.insert(0, os.path.join(bblib, "lib"))
sys.path.insert(0, os.path.join(meta, "lib"))
import bb  # noqa: E402
import bb.data  # noqa: E402
import bb.data_smart  # noqa: E402
import bb.parse  # noqa: E402
import bb.utils  # noqa: E402
import bb.fetch2  # noqa: E402
import oe  # noqa: E402
import oe.utils  # noqa: E402
import oe.license  # noqa: E402
import oe.data  # noqa: E402

bb.fetch = bb.fetch2


def top_def(path, name):
    """The text of the top-level `def name(` of a file, through its last indented line."""
    lines = open(path, encoding="utf-8").read().split("\n")
    for i, l in enumerate(lines):
        if re.match(r"def %s\(" % re.escape(name), l):
            out = [l]
            for m in lines[i + 1:]:
                if m and not m[0].isspace():
                    break
                out.append(m)
            return "\n".join(out).rstrip() + "\n"
    raise KeyError(name)


def anon_def(text):
    lines = text.rstrip("\n").split("\n")
    assert lines[0].rstrip().endswith("{") and lines[-1].strip() == "}", text
    return "def __anon(d):\n" + "\n".join(lines[1:-1]) + "\n"


# The defs base.bbclass's anonymous python calls on this release.
base_defs = []
base = os.path.join(meta, gclasses, "base.bbclass")
for n in ["set_packagetriplet"]:
    base_defs.append(top_def(base, n))
if release == "kirkstone":
    lic = os.path.join(meta, "classes", "license.bbclass")
    for n in ["check_license_format", "check_license_flags", "expand_wildcard_licenses",
              "incompatible_license", "incompatible_pkg_license", "return_spdx",
              "canonical_license", "available_licenses", "find_license_files",
              "skip_incompatible_package_licenses"]:
        try:
            base_defs.append(top_def(lic, n))
        except KeyError:
            pass

cases = json.load(open(cases_path, encoding="utf-8"))["cases"]
try:
    out = json.load(open(out_path, encoding="utf-8"))
except FileNotFoundError:
    out = {}
for c in cases:
    if c["release"] != release:
        continue
    d = bb.data_smart.DataSmart()
    d.setVar("__inherit_cache", [])
    d.setVar("PN", c["pn"])
    for k, v in c.get("oracle_only", {}).items():
        d.setVar(k, v)
    for k, v in c["vars"].items():
        d.setVar(k, v)
    for var, flags in c["flags"].items():
        for f, v in flags.items():
            d.setVarFlag(var, f, v)
    if c.get("manifest"):
        d.setVar("THISDIR", os.path.join(here, "python3-dir"))
    g = {"bb": bb, "os": os, "oe": oe, "d": d}
    if c["text"].startswith("base-"):
        # What base.bbclass's anonymous python reads besides PACKAGECONFIG: a target
        # recipe that is not skipped and is not machine specific.
        for k, v in {"LICENSE": "MIT", "SRC_URI": "", "MACHINE_ARCH": "m", "PACKAGE_ARCH": "m",
                     "WORKDIR": "/w", "S": "/w/s", "B": "/w/b", "PACKAGE_ARCHS": "all m",
                     "TARGET_OS": "linux", "TARGET_VENDOR": "-oe", "PACKAGES": "${PN}",
                     "DISTRO_FEATURES": "", "MACHINE_FEATURES": "",
                     "LICENSE_PATH": "", "COMMON_LICENSE_DIR": "/nonexistent"}.items():
            if d.getVar(k, False) is None:
                d.setVar(k, v)
        for t in base_defs:
            exec(compile(t, "base.bbclass", "exec"), g)
    for n in c["defs"]:
        exec(compile(open(os.path.join(src, n + ".txt"), encoding="utf-8").read(), n, "exec"), g)
    text = open(os.path.join(src, c["text"] + ".txt"), encoding="utf-8").read()
    exec(compile(anon_def(text), c["text"], "exec"), g)
    g["__anon"](d)
    res = {}
    for o in c["outputs"]:
        m = re.match(r"^(.*)\[(.*)\]$", o)
        if m:
            res[o] = d.getVarFlag(m.group(1), m.group(2))
        else:
            res[o] = d.getVar(o)
    out[c["id"]] = res
    print(c["id"], json.dumps(res))
with open(out_path, "w", encoding="utf-8") as f:
    json.dump(out, f, indent=1, sort_keys=True)
    f.write("\n")
