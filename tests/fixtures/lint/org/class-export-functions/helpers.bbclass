# Helpers named do_* that are no task of class mytool (linted as classes/mytool.bbclass).
python do_qa_thing() {
    bb.note("qa")
}
do_install[postfuncs] += "do_qa_thing"

mytool_do_split() {
    :
}
PACKAGEFUNCS += "mytool_do_split"

python do_write_pkg() {
    bb.note("w")
}
python do_package_write_x() {
    bb.build.exec_func("do_write_pkg", d)
}
addtask package_write_x

do_compile_extra() {
    :
}
do_compile_extra_base() {
    do_compile_extra
}

do_bundle() {
    :
}
python () {
    bb.build.addtask('do_bundle', 'do_build', 'do_install', d)
}

do_not_a_task() {
    :
}

do_install() {
    :
}
