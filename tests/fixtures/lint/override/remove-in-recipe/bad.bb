# :remove in a recipe: no bbappend or configuration can add the item back.
PACKAGECONFIG ??= "gtk x11"
PACKAGECONFIG:remove = "gtk"
SRC_URI:remove:qemux86 = "file://a.patch"
