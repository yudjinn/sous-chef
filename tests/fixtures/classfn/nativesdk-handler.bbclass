# OE-core Wrynose 33a6bd6cc8 meta/classes-recipe/nativesdk.bbclass, lines 75-108 verbatim: the
# handler and anonymous python sous-chef reproduces only when the workspace has this text
# (src/eval/classfn.rs).
python nativesdk_virtclass_handler () {
    pn = e.data.getVar("PN")
    if not (pn.endswith("-nativesdk") or pn.startswith("nativesdk-")):
        return

    oe.utils.set_class_filter("DISTRO_FEATURES", "DISTRO_FEATURES_NATIVESDK", "DISTRO_FEATURES_FILTER_NATIVESDK", d)

    e.data.setVar("MLPREFIX", "nativesdk-")
    e.data.setVar("PN", "nativesdk-" + e.data.getVar("PN").replace("-nativesdk", "").replace("nativesdk-", ""))
}

python () {
    pn = d.getVar("PN")
    if not pn.startswith("nativesdk-"):
        return

    import oe.classextend

    clsextend = oe.classextend.ClassExtender("nativesdk", [], d)
    clsextend.rename_package_variables((d.getVar("PACKAGEVARS") or "").split())

    clsextend.set_filter("DEPENDS", deps=True)
    clsextend.set_filter("PACKAGE_WRITE_DEPS", deps=False)
    clsextend.set_filter("PROVIDES", deps=False)

    if "nativesdk" in (d.getVar("BBCLASSEXTEND") or ""):
        clsextend.map_packagevars()

    d.setVar("LIBCEXTENSION", "")
    d.setVar("ABIEXTENSION", "")
}

addhandler nativesdk_virtclass_handler
nativesdk_virtclass_handler[eventmask] = "bb.event.RecipePreFinalise"
