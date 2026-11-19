# The class functions sous-chef evaluates (src/eval/classfn.rs), copied verbatim from
# OE-core Wrynose 33a6bd6cc8 (each the same on origin/kirkstone and origin/scarthgap, but
# map_kernel_arch, the same on scarthgap only): the file named before each. Only the defs
# are kept.

# meta/classes-global/utils.bbclass
def base_set_filespath(path, d):
    filespath = []
    extrapaths = (d.getVar("FILESEXTRAPATHS") or "")
    # Remove default flag which was used for checking
    extrapaths = extrapaths.replace("__default:", "")
    # Don't prepend empty strings to the path list
    if extrapaths != "":
        path = extrapaths.split(":") + path
    # The ":" ensures we have an 'empty' override
    overrides = (":" + (d.getVar("FILESOVERRIDES") or "")).split(":")
    overrides.reverse()
    for o in overrides:
        for p in path:
            if p != "":
                filespath.append(os.path.join(p, o))
    return ":".join(filespath)

# meta/classes-recipe/autotools.bbclass
def get_autotools_dep(d):
    if d.getVar('INHIBIT_AUTOTOOLS_DEPS'):
        return ''

    pn = d.getVar('PN')
    deps = ''

    if pn in ['autoconf-native', 'automake-native']:
        return deps
    deps += 'autoconf-native automake-native '

    if not pn in ['libtool', 'libtool-native'] and not pn.endswith("libtool-cross"):
        deps += 'libtool-native '
        if not bb.data.inherits_class('native', d) \
                        and not bb.data.inherits_class('nativesdk', d) \
                        and not bb.data.inherits_class('cross', d) \
                        and not d.getVar('INHIBIT_DEFAULT_DEPS'):
            deps += 'libtool-cross '

    return deps

# meta/classes-global/base.bbclass
def get_base_dep(d):
    if d.getVar('INHIBIT_DEFAULT_DEPS', False):
        return ""
    return "${BASE_DEFAULT_DEPS}"

# meta/classes-recipe/gettext.bbclass
def gettext_dependencies(d):
    if d.getVar('INHIBIT_DEFAULT_DEPS') and not oe.utils.inherits(d, 'cross-canadian'):
        return ""
    if d.getVar('USE_NLS') == 'no':
        return "gettext-minimal-native"
    return "gettext-native"

# meta/classes-recipe/gettext.bbclass
def gettext_oeconf(d):
    if d.getVar('USE_NLS') == 'no':
        return '--disable-nls'
    # Remove the NLS bits if USE_NLS is no or INHIBIT_DEFAULT_DEPS is set
    if d.getVar('INHIBIT_DEFAULT_DEPS') and not oe.utils.inherits(d, 'cross-canadian'):
        return '--disable-nls'
    return "--enable-nls"

# meta/classes-recipe/gnomebase.bbclass
def gnome_verdir(v):
    return ".".join(v.split(".")[:-1]) or v

# meta/classes-recipe/kernel-arch.bbclass
def map_kernel_arch(a, d):
    import re

    valid_archs = d.getVar('valid_archs').split()

    if   re.match('(i.86|athlon|x86.64)$', a):  return 'x86'
    elif re.match('arceb$', a):                 return 'arc'
    elif re.match('armeb$', a):                 return 'arm'
    elif re.match('aarch64$', a):               return 'arm64'
    elif re.match('aarch64_be$', a):            return 'arm64'
    elif re.match('aarch64_ilp32$', a):         return 'arm64'
    elif re.match('aarch64_be_ilp32$', a):      return 'arm64'
    elif re.match('loongarch(32|64|)$', a):     return 'loongarch'
    elif re.match('mips(isa|)(32|64|)(r6|)(el|)$', a):      return 'mips'
    elif re.match('mcf', a):                    return 'm68k'
    elif re.match('riscv(32|64|)(eb|)$', a):    return 'riscv'
    elif re.match('p(pc|owerpc)(|64)', a):      return 'powerpc'
    elif re.match('sh(3|4)$', a):               return 'sh'
    elif re.match('bfin', a):                   return 'blackfin'
    elif re.match('microblazee[bl]', a):        return 'microblaze'
    elif a in valid_archs:                      return a
    else:
        if not d.getVar("TARGET_OS").startswith("linux"):
            return a
        bb.error("cannot map '%s' to a linux kernel architecture" % a)

# meta/classes-recipe/kernel-arch.bbclass
def map_uboot_arch(a, d):
    import re

    if   re.match('p(pc|owerpc)(|64)', a): return 'ppc'
    elif re.match('i.86$', a): return 'x86'
    return a
