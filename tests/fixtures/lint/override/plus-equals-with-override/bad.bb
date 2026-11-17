# Parse-time operators on a conditional variant: they do not add to the variable.
EXTRA_OECONF:qemux86 += "--enable-qemu"
DEPENDS:class-native =+ "zlib-native"
CFLAGS:testdistro .= " -g"
RDEPENDS:${PN}:qemux86 += "bash"
EXTRA_OEMAKE:mymachine += "X=1"
