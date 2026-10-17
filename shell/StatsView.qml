pragma ComponentBehavior: Bound

import QtQuick
import qs.Commons
import qs.Ui
import "Model.js" as Model

// Overview: today, the last walk, a chart of steps or calories by period,
// and the pad below.
Column {
  id: root

  required property var panel

  readonly property var service: panel.service
  readonly property var snapshot: service ? service.state : null
  readonly property color foreground: panel.foreground
  readonly property color dim: panel.dim
  readonly property string fontFamily: panel.fontFamily

  readonly property string period: service ? service.statsPeriod : "week"
  readonly property string metric: service ? service.statsMetric : "steps"
  readonly property int offset: service ? service.statsOffset : 0
  readonly property var stats: service ? service.currentStats : null
  // Nothing to chart until a helper answers; the banner above says why.
  readonly property bool live: !!snapshot || !!stats
  readonly property var buckets: stats && stats.buckets ? stats.buckets : []
  readonly property var values: buckets.map(function(bucket) { return Model.metricValue(bucket, root.metric) })
  readonly property int currentIndex: Model.currentBucketIndex(stats, panel.now)
  readonly property int chartCursor: panel.cursorActive && panel.focusSection === "chart" ? panel.selectedIndex : -1
  readonly property int shownIndex: chartCursor >= 0 ? chartCursor
    : currentIndex >= 0 && values[currentIndex] > 0 ? currentIndex : Model.peakIndex(stats, metric)

  readonly property var today: snapshot && snapshot.today ? snapshot.today : null
  readonly property real todaySteps: today ? Number(today.steps) || 0 : 0
  readonly property var lastWalk: Model.recentSession(snapshot, panel.now.getTime() / 1000)

  readonly property var periods: [
    { value: "day", label: "Day" }, { value: "week", label: "Week" },
    { value: "month", label: "Month" }, { value: "year", label: "Year" }
  ]
  readonly property var metrics: [{ value: "steps", label: "Steps" }, { value: "calories", label: "Calories" }]

  readonly property var sections: {
    if (!live) return []
    var list = [
      { name: "period", count: periods.length, columns: periods.length },
      { name: "metric", count: metrics.length, columns: metrics.length },
      { name: "range", count: 2, columns: 2 }
    ]
    if (values.length > 0) list.push({ name: "chart", count: values.length, columns: values.length })
    return list.concat(pad.sections)
  }

  function selectPeriod(value) { if (service) service.selectPeriod(value) }
  function selectMetric(value) { if (service) service.selectMetric(value) }
  function shiftRange(delta) { if (service) service.shiftRange(delta) }

  function activate(section, index, byArrows) {
    switch (section) {
      case "period": selectPeriod(periods[index].value); break
      case "metric": selectMetric(metrics[index].value); break
      case "range": shiftRange(index === 0 ? -1 : 1); break
      default: pad.activate(section, index, byArrows)
    }
  }

  function rescan() { pad.rescan() }

  spacing: Style.space(12)

  Column {
    visible: !!root.today
    width: parent.width
    spacing: Style.space(2)

    Row {
      visible: root.todaySteps > 0
      spacing: Style.space(8)

      Text {
        id: stepsText
        textFormat: Text.PlainText
        text: Model.formatInt(root.todaySteps)
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.display
        font.bold: true
      }

      Text {
        textFormat: Text.PlainText
        anchors.baseline: stepsText.baseline
        text: root.todaySteps === 1 ? "step today" : "steps today"
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
      }
    }

    Text {
      textFormat: Text.PlainText
      width: parent.width
      text: root.todaySteps > 0 ? Model.todayLine(root.today) : "No steps yet today"
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: root.todaySteps > 0 ? Style.font.bodySmall : Style.font.subtitle
      elide: Text.ElideRight
    }

    Text {
      textFormat: Text.PlainText
      visible: !!root.lastWalk
      width: parent.width
      topPadding: Style.space(4)
      text: root.lastWalk ? "Last walk  " + Model.sessionLine(root.lastWalk) : ""
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
      elide: Text.ElideRight
    }
  }

  Item {
    visible: root.live
    width: parent.width
    height: Math.max(periodGroup.implicitHeight, metricGroup.implicitHeight)

    ButtonGroup {
      id: periodGroup
      anchors.left: parent.left
      spacing: Style.spacing.xs
      options: root.periods
      value: root.period
      focusable: false
      cursorIndex: root.panel.cursorIndexIn("period")
      foreground: root.foreground
      fontFamily: root.fontFamily
      fontSize: Style.font.bodySmall
      onChanged: function(value) { root.selectPeriod(value) }
      onHovered: function(index, on) { root.panel.hoverCursor("period", index, on) }
    }

    ButtonGroup {
      id: metricGroup
      anchors.right: parent.right
      spacing: Style.spacing.xs
      options: root.metrics
      value: root.metric
      focusable: false
      cursorIndex: root.panel.cursorIndexIn("metric")
      foreground: root.foreground
      fontFamily: root.fontFamily
      fontSize: Style.font.bodySmall
      onChanged: function(value) { root.selectMetric(value) }
      onHovered: function(index, on) { root.panel.hoverCursor("metric", index, on) }
    }
  }

  Item {
    visible: root.live
    width: parent.width
    height: previousButton.implicitHeight

    Button {
      id: previousButton
      anchors.left: parent.left
      iconText: Model.glyphs.previous
      tooltipText: "Previous " + root.period + "  ·  ,"
      hasCursor: root.panel.hasCursor("range", 0)
      foreground: root.foreground
      fontFamily: root.fontFamily
      onHovered: function(on) { root.panel.hoverCursor("range", 0, on) }
      onClicked: root.shiftRange(-1)
    }

    Text {
      textFormat: Text.PlainText
      anchors.centerIn: parent
      text: root.stats ? Model.rangeLabel(root.stats) : ""
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.body
      font.bold: true
    }

    Button {
      anchors.right: parent.right
      iconText: Model.glyphs.next
      tooltipText: "Next " + root.period + "  ·  ."
      enabled: root.offset < 0
      opacity: enabled ? 1 : 0.3
      hasCursor: root.panel.hasCursor("range", 1)
      foreground: root.foreground
      fontFamily: root.fontFamily
      onHovered: function(on) { root.panel.hoverCursor("range", 1, on) }
      onClicked: root.shiftRange(1)
    }
  }

  Column {
    visible: root.live
    width: parent.width
    spacing: Style.space(6)

    Item {
      width: parent.width
      height: valueLabel.implicitHeight

      Text {
        id: valueLabel
        textFormat: Text.PlainText
        anchors.left: parent.left
        visible: root.shownIndex >= 0
        text: root.shownIndex >= 0 ? Model.bucketLabel(root.stats, root.shownIndex) : ""
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
      }

      Text {
        textFormat: Text.PlainText
        anchors.right: parent.right
        visible: root.shownIndex >= 0
        text: root.shownIndex >= 0 ? Model.formatMetric(root.values[root.shownIndex], root.metric) : ""
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
        font.bold: true
      }
    }

    BarChart {
      width: parent.width
      values: root.values
      labels: Model.axisLabels(root.stats)
      currentIndex: root.currentIndex
      selectedIndex: root.chartCursor
      emptyText: !root.stats ? "Loading…" : Model.hasActivity(root.stats) ? "" : "No walks in this period"
      foreground: root.foreground
      fontFamily: root.fontFamily
      onHovered: function(index, on) { root.panel.hoverCursor("chart", index, on) }
    }
  }

  Column {
    visible: Model.hasActivity(root.stats)
    width: parent.width
    spacing: Style.space(2)

    Text {
      textFormat: Text.PlainText
      width: parent.width
      text: Model.summary(root.stats, root.metric)
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
      wrapMode: Text.WordWrap
    }

    Text {
      textFormat: Text.PlainText
      visible: root.metric === "calories"
      text: Model.weightHint(root.service ? root.service.weightKg : 70)
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
    }
  }

  PadSection {
    id: pad
    width: parent.width
    panel: root.panel
  }
}
