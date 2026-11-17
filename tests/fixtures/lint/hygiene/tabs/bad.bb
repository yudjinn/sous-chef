do_compile() {
	oe_runmake
    echo "indented with spaces"
}
python do_report() {
  bb.plain("two spaces")
}
def helper(d):
	return d.getVar("PN")
