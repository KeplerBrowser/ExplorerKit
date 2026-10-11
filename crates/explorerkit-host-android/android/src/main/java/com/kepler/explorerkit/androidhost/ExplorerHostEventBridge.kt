package com.kepler.explorerkit.androidhost

object ExplorerHostEventBridge {
  fun decode(json: String): ExplorerHostEvent {
    val envelope = BridgeJsonParser(json).parse().asObject()
    val payload = envelope.obj("payload")

    return when (val name = envelope.string("name")) {
      "navigationRequested" ->
        ExplorerHostEvent.NavigationRequested(
          navigationId = payload.string("navigationId"),
          url = payload.string("url")
        )
      "popupRequested" ->
        ExplorerHostEvent.PopupRequested(
          requestId = payload.string("requestId"),
          parentWebViewId = payload.string("parentWebViewId"),
          parentUrl = payload.nullableString("parentUrl"),
          targetUrl = payload.nullableString("targetUrl"),
          windowFeatures = payload.nullableString("windowFeatures"),
          policy = payload.string("policy")
        )
      "popupCreated" ->
        ExplorerHostEvent.PopupCreated(
          requestId = payload.string("requestId"),
          parentWebViewId = payload.string("parentWebViewId"),
          childWebViewId = payload.string("childWebViewId"),
          parentUrl = payload.nullableString("parentUrl"),
          targetUrl = payload.nullableString("targetUrl"),
          windowFeatures = payload.nullableString("windowFeatures"),
          policy = payload.string("policy")
        )
      "popupCreationFailed" ->
        ExplorerHostEvent.PopupCreationFailed(
          requestId = payload.string("requestId"),
          errorMessage = payload.string("errorMessage")
        )
      "urlChanged" -> ExplorerHostEvent.UrlChanged(payload.string("url"))
      "pageTitleChanged" -> ExplorerHostEvent.PageTitleChanged(payload.nullableString("title"))
      "statusTextChanged" -> ExplorerHostEvent.StatusTextChanged(payload.nullableString("status"))
      "loadStatusChanged" ->
        ExplorerHostEvent.LoadStatusChanged(payload.string("status"))
      "historyChanged" ->
        ExplorerHostEvent.HistoryChanged(
          entries = payload.array("entries").toStringList(),
          current = payload.int("current"),
          canGoBack = payload.boolean("canGoBack"),
          canGoForward = payload.boolean("canGoForward")
        )
      "closed" -> ExplorerHostEvent.Closed
      "crashed" ->
        ExplorerHostEvent.Crashed(
          url = payload.nullableString("url"),
          reason = payload.string("reason"),
          backtrace = payload.nullableString("backtrace")
        )
      "error" ->
        ExplorerHostEvent.Error(
          url = payload.nullableString("url"),
          code = payload.int("code"),
          message = payload.string("message")
        )
      "javascriptEvaluationResult" ->
        ExplorerHostEvent.JavaScriptEvaluationResult(
          evaluationId = payload.string("evaluationId"),
          ok = payload.boolean("ok"),
          valueJson = payload.nullableString("valueJson"),
          errorType = payload.nullableString("errorType")
        )
      "simpleDialogRequested" ->
        ExplorerHostEvent.SimpleDialogRequested(
          dialogId = payload.string("dialogId"),
          kind = payload.string("kind"),
          message = payload.string("message"),
          defaultValue = payload.nullableString("defaultValue")
        )
      "simpleDialogDismissed" ->
        ExplorerHostEvent.SimpleDialogDismissed(payload.string("dialogId"))
      "inputMethodRequested" ->
        ExplorerHostEvent.InputMethodRequested(
          inputMethodId = payload.string("inputMethodId"),
          type = payload.string("type"),
          text = payload.string("text"),
          insertionPoint = payload.nullableInt("insertionPoint"),
          multiline = payload.boolean("multiline"),
          allowVirtualKeyboard = payload.boolean("allowVirtualKeyboard")
        )
      "inputMethodDismissed" ->
        ExplorerHostEvent.InputMethodDismissed(payload.string("inputMethodId"))
      "selectElementRequested" ->
        ExplorerHostEvent.SelectElementRequested(
          selectElementId = payload.string("selectElementId"),
          options = parseSelectElementOptions(payload.array("options")),
          selectedOptions = payload.array("selectedOptions").toIntList(),
          allowSelectMultiple = payload.boolean("allowSelectMultiple")
        )
      "selectElementDismissed" ->
        ExplorerHostEvent.SelectElementDismissed(payload.string("selectElementId"))
      "contextMenuRequested" ->
        ExplorerHostEvent.ContextMenuRequested(
          contextMenuId = payload.string("contextMenuId"),
          x = payload.int("x"),
          y = payload.int("y"),
          width = payload.int("width"),
          height = payload.int("height"),
          elementInfo = parseContextMenuElementInformation(payload.obj("elementInfo")),
          items = parseContextMenuItems(payload.array("items"))
        )
      "contextMenuDismissed" ->
        ExplorerHostEvent.ContextMenuDismissed(payload.string("contextMenuId"))
      "filePickerRequested" ->
        ExplorerHostEvent.FilePickerRequested(
          filePickerId = payload.string("filePickerId"),
          currentPaths = payload.array("currentPaths").toStringList(),
          filterPatterns = payload.array("filterPatterns").toStringList(),
          allowSelectMultiple = payload.boolean("allowSelectMultiple")
        )
      "filePickerDismissed" -> ExplorerHostEvent.FilePickerDismissed(payload.string("filePickerId"))
      "permissionRequested" ->
        ExplorerHostEvent.PermissionRequested(
          permission = payload.string("permission"),
          origin = payload.string("origin")
        )
      "focusChanged" -> ExplorerHostEvent.FocusChanged(payload.boolean("isFocused"))
      "cursorChanged" -> ExplorerHostEvent.CursorChanged(payload.string("cursor"))
      "fullscreenChanged" -> ExplorerHostEvent.FullscreenChanged(payload.boolean("isFullscreen"))
      "surfaceAttached" ->
        ExplorerHostEvent.SurfaceAttached(
          width = payload.int("width"),
          height = payload.int("height")
        )
      "surfaceResized" ->
        ExplorerHostEvent.SurfaceResized(
          width = payload.int("width"),
          height = payload.int("height")
        )
      "surfaceDetached" -> ExplorerHostEvent.SurfaceDetached
      else -> error("Unknown host event bridge event: $name")
    }
  }

  private fun parseSelectElementOptions(
    options: BridgeJsonArray
  ): List<SelectElementOptionOrOptgroup> =
    buildList {
      for (optionOrOptgroup in options.toObjectList()) {
        when (val type = optionOrOptgroup.string("type")) {
          "option" ->
            add(SelectElementOptionOrOptgroup.Option(parseSelectElementOption(optionOrOptgroup)))
          "optgroup" ->
            add(
              SelectElementOptionOrOptgroup.Optgroup(
                label = optionOrOptgroup.string("label"),
                options = parseSelectElementOptionArray(optionOrOptgroup.array("options"))
              )
            )
          else -> error("Unknown select element option bridge type: $type")
        }
      }
    }

  private fun parseSelectElementOptionArray(options: BridgeJsonArray): List<SelectElementOption> =
    buildList {
      for (option in options.toObjectList()) {
        add(parseSelectElementOption(option))
      }
    }

  private fun parseSelectElementOption(option: BridgeJsonObject): SelectElementOption =
    SelectElementOption(
      id = option.int("id"),
      label = option.string("label"),
      isDisabled = option.boolean("isDisabled")
    )

  private fun parseContextMenuElementInformation(
    elementInfo: BridgeJsonObject
  ): ContextMenuElementInformation =
    ContextMenuElementInformation(
      isLink = elementInfo.boolean("isLink"),
      isImage = elementInfo.boolean("isImage"),
      isEditableText = elementInfo.boolean("isEditableText"),
      hasSelection = elementInfo.boolean("hasSelection"),
      linkUrl = elementInfo.nullableString("linkUrl"),
      imageUrl = elementInfo.nullableString("imageUrl"),
      contextType = elementInfo.string("contextType")
    )

  private fun parseContextMenuItems(items: BridgeJsonArray): List<ContextMenuItem> =
    buildList {
      for (item in items.toObjectList()) {
        when (val type = item.string("type")) {
          "item" ->
            add(
              ContextMenuItem.Item(
                label = item.string("label"),
                action = item.string("action"),
                enabled = item.boolean("enabled")
              )
            )
          "separator" -> add(ContextMenuItem.Separator)
          else -> error("Unknown context menu item bridge type: $type")
        }
      }
    }
}

private sealed interface BridgeJsonValue

private data class BridgeJsonObject(
  private val fields: Map<String, BridgeJsonValue>
) : BridgeJsonValue {
  fun string(name: String): String = value(name).asString()

  fun nullableString(name: String): String? =
    when (val value = fields[name] ?: BridgeJsonNull) {
      BridgeJsonNull -> null
      is BridgeJsonString -> value.value
      else -> error("Expected bridge JSON field $name to be a string or null")
    }

  fun int(name: String): Int = value(name).asInt()

  fun nullableInt(name: String): Int? =
    when (val value = fields[name] ?: BridgeJsonNull) {
      BridgeJsonNull -> null
      is BridgeJsonNumber -> value.value.toInt()
      else -> error("Expected bridge JSON field $name to be a number or null")
    }

  fun boolean(name: String): Boolean = value(name).asBoolean()

  fun obj(name: String): BridgeJsonObject = value(name).asObject()

  fun array(name: String): BridgeJsonArray = value(name).asArray()

  private fun value(name: String): BridgeJsonValue =
    fields[name] ?: error("Missing bridge JSON field $name")
}

private data class BridgeJsonArray(
  private val values: List<BridgeJsonValue>
) : BridgeJsonValue {
  fun toStringList(): List<String> = values.map { it.asString() }

  fun toIntList(): List<Int> = values.map { it.asInt() }

  fun toObjectList(): List<BridgeJsonObject> = values.map { it.asObject() }
}

private data class BridgeJsonString(val value: String) : BridgeJsonValue

private data class BridgeJsonNumber(val value: Long) : BridgeJsonValue

private data class BridgeJsonBoolean(val value: Boolean) : BridgeJsonValue

private data object BridgeJsonNull : BridgeJsonValue

private fun BridgeJsonValue.asObject(): BridgeJsonObject =
  this as? BridgeJsonObject ?: error("Expected bridge JSON value to be an object")

private fun BridgeJsonValue.asArray(): BridgeJsonArray =
  this as? BridgeJsonArray ?: error("Expected bridge JSON value to be an array")

private fun BridgeJsonValue.asString(): String =
  (this as? BridgeJsonString)?.value ?: error("Expected bridge JSON value to be a string")

private fun BridgeJsonValue.asInt(): Int =
  (this as? BridgeJsonNumber)?.value?.toInt() ?: error("Expected bridge JSON value to be a number")

private fun BridgeJsonValue.asBoolean(): Boolean =
  (this as? BridgeJsonBoolean)?.value ?: error("Expected bridge JSON value to be a boolean")

private class BridgeJsonParser(private val input: String) {
  private var index = 0

  fun parse(): BridgeJsonValue {
    val value = parseValue()
    skipWhitespace()
    if (index != input.length) {
      error("Unexpected bridge JSON content at offset $index")
    }
    return value
  }

  private fun parseValue(): BridgeJsonValue {
    skipWhitespace()
    if (index >= input.length) {
      error("Unexpected end of bridge JSON")
    }

    return when (input[index]) {
      '{' -> parseObject()
      '[' -> parseArray()
      '"' -> BridgeJsonString(parseString())
      't' -> {
        expectLiteral("true")
        BridgeJsonBoolean(true)
      }
      'f' -> {
        expectLiteral("false")
        BridgeJsonBoolean(false)
      }
      'n' -> {
        expectLiteral("null")
        BridgeJsonNull
      }
      '-' -> BridgeJsonNumber(parseNumber())
      in '0'..'9' -> BridgeJsonNumber(parseNumber())
      else -> error("Unexpected bridge JSON character ${input[index]} at offset $index")
    }
  }

  private fun parseObject(): BridgeJsonObject {
    expect('{')
    skipWhitespace()
    val fields = linkedMapOf<String, BridgeJsonValue>()
    if (consume('}')) {
      return BridgeJsonObject(fields)
    }

    while (true) {
      skipWhitespace()
      val name = parseString()
      skipWhitespace()
      expect(':')
      fields[name] = parseValue()
      skipWhitespace()
      if (consume('}')) {
        return BridgeJsonObject(fields)
      }
      expect(',')
    }
  }

  private fun parseArray(): BridgeJsonArray {
    expect('[')
    skipWhitespace()
    val values = mutableListOf<BridgeJsonValue>()
    if (consume(']')) {
      return BridgeJsonArray(values)
    }

    while (true) {
      values += parseValue()
      skipWhitespace()
      if (consume(']')) {
        return BridgeJsonArray(values)
      }
      expect(',')
    }
  }

  private fun parseString(): String {
    expect('"')
    val value = StringBuilder()

    while (index < input.length) {
      when (val character = input[index++]) {
        '"' -> return value.toString()
        '\\' -> value.append(parseEscapedCharacter())
        else -> value.append(character)
      }
    }

    error("Unterminated bridge JSON string")
  }

  private fun parseEscapedCharacter(): Char {
    if (index >= input.length) {
      error("Unterminated bridge JSON escape sequence")
    }

    return when (val escaped = input[index++]) {
      '"' -> '"'
      '\\' -> '\\'
      '/' -> '/'
      'b' -> '\b'
      'f' -> '\u000c'
      'n' -> '\n'
      'r' -> '\r'
      't' -> '\t'
      'u' -> parseUnicodeEscape()
      else -> error("Unsupported bridge JSON escape sequence \\$escaped at offset ${index - 1}")
    }
  }

  private fun parseUnicodeEscape(): Char {
    var value = 0
    repeat(4) {
      if (index >= input.length) {
        error("Unterminated bridge JSON unicode escape")
      }
      val digit =
        input[index++].digitToIntOrNull(16)
          ?: error("Invalid bridge JSON unicode escape at offset ${index - 1}")
      value = value * 16 + digit
    }
    return value.toChar()
  }

  private fun parseNumber(): Long {
    val start = index
    if (input[index] == '-') {
      index += 1
    }
    if (index >= input.length || input[index] !in '0'..'9') {
      error("Invalid bridge JSON number at offset $start")
    }
    if (input[index] == '0') {
      index += 1
    } else {
      while (index < input.length && input[index] in '0'..'9') {
        index += 1
      }
    }
    return input.substring(start, index).toLong()
  }

  private fun expectLiteral(literal: String) {
    if (!input.startsWith(literal, index)) {
      error("Expected bridge JSON literal $literal at offset $index")
    }
    index += literal.length
  }

  private fun expect(expected: Char) {
    if (index >= input.length || input[index] != expected) {
      error("Expected bridge JSON character $expected at offset $index")
    }
    index += 1
  }

  private fun consume(expected: Char): Boolean {
    if (index >= input.length || input[index] != expected) {
      return false
    }
    index += 1
    return true
  }

  private fun skipWhitespace() {
    while (index < input.length && input[index].isWhitespace()) {
      index += 1
    }
  }
}
