# Task functions of class mytool (the fixture test lints this as classes/mytool.bbclass).
mytool_do_compile() {
    oe_runmake
}

mytool_do_install() {
    oe_runmake install
}
EXPORT_FUNCTIONS do_compile

do_configure() {
    ./configure
}
