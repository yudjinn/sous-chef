SUMMARY = "Clean continuations"
DEPENDS = "zlib \
    openssl"

# A comment between statements.
RDEPENDS:${PN} = "bash \
    coreutils \
    "

do_install() {
	install -d ${D}${bindir} \   
	    ${D}${sysconfdir}

}
