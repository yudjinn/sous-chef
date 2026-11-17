SUMMARY = "GNU Hello stand-in"
DESCRIPTION = "Prints a friendly greeting. Shared by every hello version."
HOMEPAGE = "https://example.org/hello"
SECTION = "examples"
LICENSE = "MIT"

DEPENDS = "virtual/foo"

SRC_URI = "https://example.org/releases/hello-${PV}.tar.gz"

inherit autotoolsish

EXTRA_OECONF = "--disable-nls"

do_install:append() {
	install -d ${D}${sysconfdir}
	echo "greeting=hello" > ${D}${sysconfdir}/hello.conf
}

FILES:${PN} += "${sysconfdir}/hello.conf"
