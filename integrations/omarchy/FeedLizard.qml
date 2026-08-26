import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Quickshell
import qs.Commons
import qs.Ui

Panel {
  id: root
  moduleName: "io.github.feedlizard.bar"
  ipcTarget: "io.github.feedlizard.bar"
  manageIpc: false

  readonly property color foreground: bar ? bar.foreground : Color.foreground
  readonly property color dim: Qt.darker(foreground, 1.45)
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  FeedLizardService { id: service }

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    iconComponent: Component {
      Row {
        spacing: Style.space(4)
        Image {
          anchors.verticalCenter: parent.verticalCenter
          source: Qt.resolvedUrl("feedlizard.svg")
          sourceSize.width: Style.space(14)
          sourceSize.height: Style.space(14)
          width: Style.space(14)
          height: Style.space(14)
          fillMode: Image.PreserveAspectFit
        }
        Text {
          anchors.verticalCenter: parent.verticalCenter
          visible: service.available && service.totalUnread > 0 && !(bar && bar.vertical)
          text: service.totalUnread > 9999 ? "9999+" : String(service.totalUnread)
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
          font.weight: Font.DemiBold
        }
      }
    }
    tooltipText: service.available
      ? (service.totalUnread === 1 ? "1 unread FeedLizard article" : service.totalUnread + " unread FeedLizard articles")
      : "Open FeedLizard"
    onPressed: function(buttonCode) {
      if (buttonCode === Qt.RightButton || buttonCode === Qt.MiddleButton) service.refresh()
      else root.toggle()
    }
  }

  KeyboardPanel {
    id: panel
    anchorItem: button
    owner: root
    bar: root.bar
    open: root.opened
    focusTarget: keyCatcher
    contentWidth: panel.fittedContentWidth(Style.space(330))
    contentHeight: panel.fittedContentHeight(content.implicitHeight, Style.space(440))

    PanelKeyCatcher {
      id: keyCatcher
      anchors.fill: parent
      onCloseRequested: root.close()
      onTextKey: function(text) {
        if (text === "r" || text === "R") service.refresh()
      }
    }

    ColumnLayout {
      id: content
      anchors.fill: parent
      anchors.margins: Style.space(16)
      spacing: Style.space(10)

      RowLayout {
        Layout.fillWidth: true
        spacing: Style.space(10)
        Image {
          source: Qt.resolvedUrl("feedlizard.svg")
          sourceSize.width: Style.space(28)
          sourceSize.height: Style.space(28)
          Layout.preferredWidth: Style.space(28)
          Layout.preferredHeight: Style.space(28)
        }
        ColumnLayout {
          Layout.fillWidth: true
          spacing: 0
          Text {
            text: "FeedLizard"
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.title
            font.weight: Font.DemiBold
          }
          Text {
            text: service.available
              ? (service.totalUnread === 1 ? "1 unread" : service.totalUnread + " unread")
              : "FeedLizard is not running"
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
          }
        }
      }

      Repeater {
        model: service.folders
        delegate: RowLayout {
          required property var modelData
          Layout.fillWidth: true
          Text {
            Layout.fillWidth: true
            text: modelData.name
            color: root.foreground
            elide: Text.ElideRight
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
          }
          Text {
            text: String(modelData.unread)
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
          }
        }
      }

      Text {
        visible: service.available && service.totalUnread === 0
        text: "You’re all caught up."
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
      }

      RowLayout {
        Layout.fillWidth: true
        spacing: Style.space(8)
        Button {
          Layout.fillWidth: true
          text: "Open Unread"
          enabled: service.available
          onClicked: { service.openUnread(); root.close() }
        }
        Button {
          Layout.fillWidth: true
          text: "Open FeedLizard"
          onClicked: { service.openFeedLizard(); root.close() }
        }
      }
      Button {
        Layout.fillWidth: true
        text: "Refresh"
        enabled: service.available
        onClicked: service.refresh()
      }
    }
  }
}
