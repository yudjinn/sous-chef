# += where it does; :append where it is needed.
DEPENDS = "zlib"
DEPENDS += "openssl"
EXTRA_OECONF:append = " --no-earlier-set"
SRC_URI = "file://a.patch"
SRC_URI:append:qemux86 = " file://b.patch"
RDEPENDS:${PN} = "bash"
RDEPENDS:${PN}:append = " perl"
MY_LATE = "a"
MY_LATE:append = " b"
MY_LATE = "c"
CFLAGS = "-O2"
CFLAGS:append = " -g"
inherit autotoolsish
