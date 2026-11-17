do_compile() {
	oe_runmake \
	    GREETING=hello
	cat > ${B}/x.conf <<EOF
    indented = "with spaces, as content"
EOF
	echo done
}

do_install() {
    install -d ${D}${bindir}
    install -m 0755 app ${D}${bindir}
}

python do_report() {
    # a comment
    if d.getVar("PN"):
        bb.plain("%s %s" % (d.getVar("PN"),
                             d.getVar("PV")))
}

def helper(d):
    return d.getVar("PN")
