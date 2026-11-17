# Task functions of class mytool (the fixture test lints this as classes/mytool.bbclass).
mytool_do_compile() {
    oe_runmake
}

python mytool_do_install() {
    bb.build.exec_func("oe_runmake", d)
}
EXPORT_FUNCTIONS do_compile do_install

addtask mytask after do_compile before do_build
do_mytask() {
    echo mytask
}

do_install:append() {
    echo extra
}
mytool_helper() {
    :
}
