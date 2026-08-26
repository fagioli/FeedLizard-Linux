import QtQuick
import Quickshell.Io

Item {
  id: root

  readonly property string helperPath: Qt.resolvedUrl("bin/feedlizard-ipc").toString().replace(/^file:\/\//, "")
  property bool available: false
  property int totalUnread: 0
  property var folders: []
  property string lastError: ""

  function applyState(text) {
    try {
      var state = JSON.parse(String(text || "{}"))
      if (state.protocol_version !== 1 || !Array.isArray(state.folders)) throw new Error("unsupported response")
      totalUnread = Math.max(0, Number(state.total_unread || 0))
      folders = state.folders.slice(0, 5)
      available = true
      lastError = ""
    } catch (error) {
      available = false
      totalUnread = 0
      folders = []
      lastError = "FeedLizard integration response was invalid"
    }
  }

  function requestState() {
    if (!stateProcess.running) stateProcess.running = true
  }

  function invoke(method) {
    if (actionProcess.running) return
    actionProcess.command = [helperPath, "call", method]
    actionProcess.running = true
  }

  function openFeedLizard() {
    if (available) invoke("OpenFeedLizard")
    else if (!launchProcess.running) launchProcess.running = true
  }

  function openUnread() { invoke("OpenUnread") }
  function refresh() { invoke("Refresh") }

  Component.onCompleted: requestState()

  Process {
    id: stateProcess
    running: false
    command: [root.helperPath, "state"]
    stdout: StdioCollector {
      id: stateOutput
      waitForEnd: true
    }
    onExited: function(exitCode) {
      if (exitCode === 0) root.applyState(stateOutput.text)
      else {
        root.available = false
        root.totalUnread = 0
        root.folders = []
      }
    }
  }

  Process {
    id: monitorProcess
    running: true
    command: [root.helperPath, "monitor"]
    stdout: SplitParser {
      onRead: function(line) {
        if (String(line).indexOf("UnreadChanged") >= 0 || String(line).indexOf("The name") >= 0)
          root.requestState()
      }
    }
    onExited: function(exitCode) {
      root.available = false
      root.totalUnread = 0
      root.folders = []
    }
  }

  Process {
    id: actionProcess
    running: false
    command: []
    onExited: function(exitCode) {
      if (exitCode === 0) root.requestState()
      else root.available = false
    }
  }

  Process {
    id: launchProcess
    running: false
    command: ["gtk-launch", "io.github.feedlizard.FeedLizard"]
  }
}
