addhandler my_handler
my_handler[eventmask] = "bb.event.BuildStarted"
python my_handler() {
    bb.plain("build started")
}
