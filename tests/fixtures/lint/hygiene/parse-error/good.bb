SUMMARY = "hygiene/parse-error: good recipe"
LICENSE = "MIT"
# The whitespace after the backslash below is hygiene/continuation's finding, not this rule's.
DEPENDS = "a \  
    b"
addtask do_x after do_a do_c before do_b
inherit_defer native

do_install() {
    install -d ${D}${bindir}
}

python do_check() {
    bb.note("ok")
}
