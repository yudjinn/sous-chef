"""Runs expressions through the real BitBake / OE-core helpers and writes code, the
variables (JSON) and str(result) (or ERROR:<exception>) as TSV, for tests/pyexpr_oracle.rs.

usage: oracle.py <bitbake/lib> <openembedded-core/meta/lib> <out.tsv>
"""
import itertools
import json
import re
import sys

sys.path.insert(0, sys.argv[1])
sys.path.insert(0, sys.argv[2])
import bb  # noqa: E402
import bb.utils  # noqa: E402
import oe  # noqa: E402
import oe.utils  # noqa: E402
import oe.types  # noqa: E402


class D:
    def __init__(self, vars, flags):
        self.vars = vars
        self.flags = flags

    # The signatures of BitBake 2.18 data_smart.py (keyword names matter).
    def getVar(self, var, expand=True, noweakdefault=False, parsing=False):
        return self.vars.get(var)

    def getVarFlag(self, var, flag, expand=True, noweakdefault=False, parsing=False,
                   retparser=False):
        return self.flags.get(var + "[" + flag + "]")

    def expand(self, s, varname=None):
        return re.sub(r"\$\{([^}]+)\}", lambda m: self.vars.get(m.group(1), m.group(0)), s)


VALUES = [None, "", "a", "a b", " a  b\tc ", "x11 wayland opengl", "Yes", "no", "0", "1", "T",
          "maybe", "1.5", "4.10", "10", "2:1.0-r1", "1.0~rc1", "lib32-zlib-native", "b a"]
CHECKS = ["'a'", "'a b'", "'b c'", "''", "'x11 opengl'", "'zz'", "['a', 'b']", "[]", "['a', 1]",
          "None", "1", "'a a'"]

codes = []
for c in CHECKS:
    codes.append(f"bb.utils.contains('V', {c}, 'yes', 'no', d)")
    codes.append(f"bb.utils.contains_any('V', {c}, 'yes', 'no', d)")
    codes.append(f"bb.utils.filter('V', {c}, d)")
    codes.append(f"oe.utils.both_contain('V', 'W', {c}, d)")
    codes.append(f"oe.utils.all_distro_features(d, {c})")
    codes.append(f"oe.utils.any_distro_features(d, {c}, 'y', falsevalue='n')")
for chk in ["'a'", "''", "'1'", "'no'", "None"]:
    codes.append(f"oe.utils.conditional('V', {chk}, 'T', 'F', d)")
codes += [
    "bb.utils.to_boolean(d.getVar('V'))",
    "bb.utils.to_boolean(d.getVar('V'), 'dflt')",
    "oe.utils.vartrue('V', 'T', 'F', d)",
    "oe.utils.ifelse(d.getVar('V'))",
    "oe.utils.ifelse(d.getVar('V'), 'T', iffalse='F')",
    "oe.utils.less_or_equal('V', '4.5', 'T', 'F', d)",
    "oe.utils.less_or_equal('V', 2, 'T', 'F', d)",
    "oe.utils.version_less_or_equal('V', '4.9', 'T', 'F', d)",
    "oe.utils.version_less_or_equal('V', '1.0', 'T', 'F', d)",
    "bb.utils.vercmp_string(d.getVar('V'), d.getVar('W'))",
    "oe.utils.prune_suffix(d.getVar('V'), ['-native', 'b'], d)",
    "oe.utils.prune_suffix(d.getVar('V'), d.getVar('W').split(), d)",
    "oe.utils.set_difference('V', 'W', d)",
    "oe.utils.filter_default_features('X', d)",
    "d.getVar('V') or 'none'",
    "d.getVar('V') and 'set'",
    "not d.getVar('V')",
    "'T' if d.getVar('V') else 'F'",
    "'T' if d.getVar('V') == 'a' else 'F'",
    "d.getVar('V') != d.getVar('W')",
    "'a' in d.getVar('V')",
    "'a' in d.getVar('V').split()",
    "'a' not in d.getVar('V').split()",
    "d.getVar('V').split()",
    "' '.join(d.getVar('V').split())",
    "'-'.join(d.getVar('V'))",
    "d.getVar('V').split('a')",
    "d.getVar('V').split(' ')[0]",
    "d.getVar('V').split()[-1]",
    "d.getVar('V').strip()",
    "d.getVar('V').strip(' a')",
    "d.getVar('V').replace(' ', '_')",
    "d.getVar('V').replace('', '.')",
    "d.getVar('V').startswith('a')",
    "d.getVar('V').endswith('b')",
    "d.getVar('V').lower() + d.getVar('V').upper()",
    "len(d.getVar('V'))",
    "len(d.getVar('V').split())",
    "['', 'yes:'][d.getVar('V') != '']",
    "['n', 'y'][bool(d.getVar('V'))]",
    "d.getVar('V') + d.getVar('W')",
    "d.getVar('V') == d.getVar('W') == 'a'",
    "d.getVarFlag('V', 'f')",
    "d.expand('${V}-${W}')",
    "d.getVar('V', True)",
    "d.getVar('V', expand=True)",
    "bb.utils.contains('V', 'a', True, False, d)",
    "bb.utils.contains('V', 'a', 1, 0, d)",
    "bb.utils.contains(variable='V', checkvalues='a', truevalue='T', falsevalue='F', d=d)",
    "d.getVar('V').replace('a', 'b', 1)",
    "None",
    "True and 'x'",
    "0 or ''",
    "-1",
    "[1, 2][-1]",
    "'-%s' % d.getVar('V')",
    "'%s:%s' % (d.getVar('V'), d.getVar('W'))",
    "'%d' % len(d.getVar('V'))",
    "'%s%%' % d.getVar('V')",
    "('-%s' % d.getVar('V').split()[-1].replace('a', 'z')) if (d.getVar('V') != '') else ''",
    "'%s' % (d.getVar('V'),)",
    "'%s %s' % d.getVar('V')",
    "[d.getVar('W'), d.getVar('V')][bool(d.getVar('V'))].replace('-', '_')",
    "bool(d.getVar('V'))",
    "bool()",
    "len(obj=d.getVar('V'))",
    "bool(x=d.getVar('V'))",
    "d.getVar(var='V')",
    "bb.utils.to_boolean(string=d.getVar('V'))",
]

out = open(sys.argv[3], "w")
pairs = list(itertools.product(VALUES, ["a b", None, "", "a", "2:1.0-r0", "4.9"]))
out.write("#V\t" + "\t".join(json.dumps(v) for v, _ in pairs) + "\n")
out.write("#W\t" + "\t".join(json.dumps(w) for _, w in pairs) + "\n")
for code in codes:
    row = [code]
    for v, w in pairs:
        vars = {"DISTRO_FEATURES": "a x11 opengl", "MLPREFIX": "lib32-",
                "X_DEFAULTS": "c a b a", "X_OPTED_OUT": "b"}
        if v is not None:
            vars["V"] = v
        if w is not None:
            vars["W"] = w
        flags = {"V[f]": v} if v is not None else {}
        d = D(vars, flags)
        try:
            ctx = {"bb": bb, "oe": oe, "d": d}
            r = str(eval(compile(code.strip(), "<expansion>", "eval"), ctx, {"d": d}))
        except Exception as e:  # noqa: BLE001
            r = "ERROR:" + type(e).__name__
        # str(result) as a JSON string (tabs and newlines escaped), or ERROR:<exception>.
        row.append(r if r.startswith("ERROR:") else json.dumps(r))
    out.write("\t".join(row) + "\n")
out.close()
