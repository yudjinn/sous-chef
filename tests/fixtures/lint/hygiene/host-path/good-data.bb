# Absolute paths that are data, not host access (B13): from OE-core Wrynose 33a6bd6c.
SUMMARY = "Paths written as text"
LICENSE = "MIT"

do_compile() {
	# busybox.inc:206-208: a grep pattern.
	if grep -q -x "/bin/sh" busybox.links.suid; then
		bbfatal "busybox suid binary incorrectly provides /bin/sh"
	fi
}

do_install() {
	install -d ${D}${sysconfdir}/default/volatiles
	# glibc-package.inc:143: a line of text written under ${D}.
	echo "d root root 0755 /var/run/nscd none" \
		> ${D}${sysconfdir}/default/volatiles/98_nscd
	# initscripts_1.0.bb:106: a sed expression.
	sed -i -e '\@^d root root 0755 /var/volatile/log none$@ a\l root root 0755 /var/log /var/volatile/log' \
		${D}${sysconfdir}/default/volatiles/00_core
	# ncurses.inc:194: a symlink's target, resolved on the target.
	ln -s /etc/terminfo/x/xterm \
		${D}${datadir}/terminfo/x/xterm
	# help2man_1.49.3.bb:17: a shebang.
	sed -i -e "1s:#!.*:#! /usr/bin/env perl:" ${D}${bindir}/help2man
	printf '#!/bin/sh\nexec true\n' > ${D}${bindir}/wrapper
}

do_install_ptest() {
	# gawk_5.4.0.bb:77-79: sed expressions on continuation lines.
	sed -i \
	    -e 's|#! /bin/gawk|#! ${bindir}/gawk|g' \
	    -e 's|#! /usr/local/bin/gawk|#! ${bindir}/gawk|g' ${D}${PTEST_PATH}/test/*.awk
}
