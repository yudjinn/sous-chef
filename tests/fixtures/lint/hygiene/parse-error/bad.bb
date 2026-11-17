SUMMARY = "hygiene/parse-error: bad recipe"
  # an indented comment is an unparsed line in BitBake
this is not bitbake
addtask do_x after do_a before do_b after do_c
addpylib ${LAYERDIR}/lib oe

do_broken() {
    echo "never closed"
