SUMMARY = "Syntax every supported release has"
SRC_URI = "file://main.c file://extra.cfg;subdir=conf"
inherit ${PROVIDER_CLASS}
include conf/extra-features.inc
# inherit_defer and include_all would need scarthgap and walnascar.
INHERIT_DEFER_NOTE = "inherit_defer is a word in a value here"

do_compile() {
	${CC} ${CFLAGS} ${S}/main.c -o main
	cp ${S}/conf/extra.cfg ${B}/
	cp ${WORKDIR}/main.c.orig ${B}/ || true
}
