# Parse-time operators on a conditional variant: they do not add to the variable.
EXTRA_OECONF:append:qemux86 = " --enable-qemu"
DEPENDS:prepend:class-native = "zlib-native "
CFLAGS:append:testdistro = " -g"
RDEPENDS:${PN}:append:qemux86 = " bash"
EXTRA_OEMAKE:append:mymachine = " X=1"
