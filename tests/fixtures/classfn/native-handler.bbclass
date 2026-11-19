# OE-core Wrynose 33a6bd6cc8 meta/classes-recipe/native.bbclass, lines 122-166 verbatim: the
# handler sous-chef reproduces only when the workspace has this text (src/eval/classfn.rs).
python native_virtclass_handler () {
    import re
    pn = d.getVar("PN")
    if not pn.endswith("-native"):
        return
    bpn = d.getVar("BPN")

    oe.utils.set_class_filter("DISTRO_FEATURES", "DISTRO_FEATURES_NATIVE", "DISTRO_FEATURES_FILTER_NATIVE", d)

    classextend = d.getVar('BBCLASSEXTEND') or ""
    if "native" not in classextend:
        return

    def map_dependencies(varname, d, suffix, selfref=True, regex=False):
        varname = varname + ":" + suffix
        # Handle ${PN}-xxx -> ${BPN}-xxx-native
        if suffix != "${PN}" and "${PN}" in suffix:
            output_varname = varname.replace("${PN}", "${BPN}") + "-native"
            d.renameVar(varname, output_varname)

    d.setVarFilter("DEPENDS", "native_filter(val, '" + pn + "', '" + bpn + "', selfref=False)")

    for varname in ["RDEPENDS", "RRECOMMENDS", "RSUGGESTS", "RPROVIDES", "RREPLACES"]:
        d.setVarFilter(varname, "native_filter(val, '" + pn + "', '" + bpn + "')")

    # We need to handle things like ${@bb.utils.contains('PTEST_ENABLED', '1', '${PN}-ptest', '', d)}
    # and not pass ${PN}-test since in the native case it would be ignored. This does mean we ignore
    # anonymous python derived PACKAGES entries.
    for pkg in re.split(r"\${@(?:{.*?}|.)+?}|\s", d.getVar("PACKAGES", False)):
        if not pkg:
            continue
        map_dependencies("RDEPENDS", d, pkg)
        map_dependencies("RRECOMMENDS", d, pkg)
        map_dependencies("RSUGGESTS", d, pkg)
        map_dependencies("RPROVIDES", d, pkg)
        map_dependencies("RREPLACES", d, pkg)

    d.setVarFilter("PACKAGES", "native_filter(val, '" + pn + "', '" + bpn + "')")
    d.setVarFilter("PACKAGES_DYNAMIC", "native_filter(val, '" + pn + "', '" + bpn + "', regex=True)")

    d.setVarFilter("PROVIDES", "native_filter(val, '" + pn + "', '" + bpn + "')")
}

addhandler native_virtclass_handler
native_virtclass_handler[eventmask] = "bb.event.RecipePreFinalise"
