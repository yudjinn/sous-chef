SUMMARY = "Local files through WORKDIR"
SRC_URI = "file://main.c file://app.conf file://extra.cfg;subdir=conf"

do_compile() {
	${CC} ${CFLAGS} ${UNPACKDIR}/main.c -o main
	cp ${WORKDIR}/main.c.orig ${B}/ || true
}

do_install() {
	install -m 0644 ${UNPACKDIR}/app.conf ${D}${sysconfdir}/app.conf
	sed -i -e "s:@WORK@:${WORKDIR}:" ${D}${sysconfdir}/app.conf
	install -m 0644 ${UNPACKDIR}/app.conf ${D}${sysconfdir}/app2.conf
}
