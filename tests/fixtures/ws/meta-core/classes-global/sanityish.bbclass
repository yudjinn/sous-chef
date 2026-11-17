# Stand-in for sanity.bbclass: a global class, pulled in through INHERIT.

SANITY_REQUIRED_UTILITIES ?= "patch diffstat git"

addhandler sanityish_check
sanityish_check[eventmask] = "bb.event.ConfigParsed"
python sanityish_check() {
    if not e.data.getVar('MACHINE'):
        bb.fatal("MACHINE is not set")
}
