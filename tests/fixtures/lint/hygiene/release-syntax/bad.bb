SUMMARY = "Syntax from newer and older releases"
SRC_URI = "file://main.c file://extra.cfg;subdir=conf"
inherit_defer ${PROVIDER_CLASS}
include_all conf/extra-features.inc

do_compile() {
	${CC} ${CFLAGS} ${WORKDIR}/main.c -o main
	cp ${UNPACKDIR}/conf/extra.cfg ${B}/
}
