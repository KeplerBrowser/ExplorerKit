package com.kepler.explorerkit.androidexample

import com.kepler.explorerkit.androidhost.ContextMenuElementInformation
import com.kepler.explorerkit.androidhost.ContextMenuItem
import com.kepler.explorerkit.androidhost.SelectElementOption
import com.kepler.explorerkit.androidhost.SelectElementOptionOrOptgroup
import com.kepler.explorerkit.androidhost.ExplorerHostEvent

/** Shared ExplorerKit host event envelope adapted for the Android example chrome. */
data class NativeHostEvent(val event: ExplorerHostEvent) {
  val name: String
    get() = event.hostEventName()

  fun urlChanged(): String? = (event as? ExplorerHostEvent.UrlChanged)?.url

  fun navigationRequest(): NativeNavigationRequest? =
    (event as? ExplorerHostEvent.NavigationRequested)?.let {
      NativeNavigationRequest(
        navigationId = it.navigationId,
        url = it.url
      )
    }

  fun simpleDialogRequest(): NativeSimpleDialogRequest? =
    (event as? ExplorerHostEvent.SimpleDialogRequested)?.let {
      NativeSimpleDialogRequest(
        dialogId = it.dialogId,
        kind = it.kind,
        message = it.message,
        defaultValue = it.defaultValue
      )
    }

  fun simpleDialogDismissedId(): String? =
    (event as? ExplorerHostEvent.SimpleDialogDismissed)?.dialogId

  fun inputMethodRequest(): NativeInputMethodRequest? =
    (event as? ExplorerHostEvent.InputMethodRequested)?.let {
      NativeInputMethodRequest(
        inputMethodId = it.inputMethodId,
        type = it.type,
        text = it.text,
        insertionPoint = it.insertionPoint,
        multiline = it.multiline,
        allowVirtualKeyboard = it.allowVirtualKeyboard
      )
    }

  fun inputMethodDismissedId(): String? =
    (event as? ExplorerHostEvent.InputMethodDismissed)?.inputMethodId

  fun selectElementRequest(): NativeSelectElementRequest? =
    (event as? ExplorerHostEvent.SelectElementRequested)?.let {
      NativeSelectElementRequest(
        selectElementId = it.selectElementId,
        options = it.options.map(::toNativeSelectElementOptionOrOptgroup),
        selectedOptions = it.selectedOptions,
        allowSelectMultiple = it.allowSelectMultiple
      )
    }

  fun selectElementDismissedId(): String? =
    (event as? ExplorerHostEvent.SelectElementDismissed)?.selectElementId

  fun contextMenuRequest(): NativeContextMenuRequest? =
    (event as? ExplorerHostEvent.ContextMenuRequested)?.let {
      NativeContextMenuRequest(
        contextMenuId = it.contextMenuId,
        x = it.x,
        y = it.y,
        width = it.width,
        height = it.height,
        elementInfo = it.elementInfo.toNativeContextMenuElementInformation(),
        items = it.items.map(::toNativeContextMenuItem)
      )
    }

  fun contextMenuDismissedId(): String? =
    (event as? ExplorerHostEvent.ContextMenuDismissed)?.contextMenuId

  fun filePickerRequest(): NativeFilePickerRequest? =
    (event as? ExplorerHostEvent.FilePickerRequested)?.let {
      NativeFilePickerRequest(
        filePickerId = it.filePickerId,
        currentPaths = it.currentPaths,
        filterPatterns = it.filterPatterns,
        allowSelectMultiple = it.allowSelectMultiple
      )
    }

  fun filePickerDismissedId(): String? =
    (event as? ExplorerHostEvent.FilePickerDismissed)?.filePickerId

  fun permissionRequest(): NativePermissionRequest? =
    (event as? ExplorerHostEvent.PermissionRequested)?.let {
      NativePermissionRequest(
        permission = it.permission,
        origin = it.origin
      )
    }

  fun describe(): String =
    when (val event = event) {
      is ExplorerHostEvent.NavigationRequested ->
        "navigation id=${event.navigationId} url=${event.url}"
      is ExplorerHostEvent.UrlChanged -> "url=${event.url}"
      is ExplorerHostEvent.LoadStatusChanged -> "load=${event.status}"
      is ExplorerHostEvent.PageTitleChanged -> "title=${event.title}"
      is ExplorerHostEvent.StatusTextChanged -> "status=${event.status}"
      is ExplorerHostEvent.HistoryChanged ->
        "history current=${event.current} canGoBack=${event.canGoBack} canGoForward=${event.canGoForward} entries=${event.entries.size}"
      is ExplorerHostEvent.SimpleDialogRequested ->
        "dialog id=${event.dialogId} kind=${event.kind} message=${event.message}"
      is ExplorerHostEvent.SimpleDialogDismissed -> "dialog dismissed id=${event.dialogId}"
      is ExplorerHostEvent.InputMethodRequested ->
        "ime requested id=${event.inputMethodId} type=${event.type} multiline=${event.multiline} textLength=${event.text.length} virtualKeyboard=${event.allowVirtualKeyboard}"
      is ExplorerHostEvent.InputMethodDismissed -> "ime dismissed id=${event.inputMethodId}"
      is ExplorerHostEvent.SelectElementRequested ->
        selectElementRequest()?.let {
          val groupLabels = it.optgroupLabels().joinToString(prefix = "[", postfix = "]")
          "select requested id=${it.selectElementId} multiple=${it.allowSelectMultiple} selected=${it.selectedOptions} options=${it.optionCount()} disabled=${it.disabledOptionCount()} optgroups=$groupLabels"
        } ?: "selectElementRequested"
      is ExplorerHostEvent.SelectElementDismissed -> "select dismissed id=${event.selectElementId}"
      is ExplorerHostEvent.ContextMenuRequested ->
        contextMenuRequest()?.let {
          "context menu requested id=${it.contextMenuId} type=${it.elementInfo.contextType} at=${it.x},${it.y} size=${it.width}x${it.height} link=${it.elementInfo.isLink} image=${it.elementInfo.isImage} editable=${it.elementInfo.isEditableText} selection=${it.elementInfo.hasSelection} items=${it.itemCount()} enabled=${it.enabledItemCount()}"
        } ?: "contextMenuRequested"
      is ExplorerHostEvent.ContextMenuDismissed ->
        "context menu dismissed id=${event.contextMenuId}"
      is ExplorerHostEvent.FilePickerRequested ->
        "file picker requested id=${event.filePickerId} multiple=${event.allowSelectMultiple} filters=${event.filterPatterns} currentPaths=${event.currentPaths.size}"
      is ExplorerHostEvent.FilePickerDismissed ->
        "file picker dismissed id=${event.filePickerId}"
      is ExplorerHostEvent.PermissionRequested ->
        "permission requested kind=${event.permission} origin=${event.origin}"
      is ExplorerHostEvent.FocusChanged -> "focus=${event.isFocused}"
      is ExplorerHostEvent.SurfaceAttached -> "surfaceAttached ${event.width}x${event.height}"
      is ExplorerHostEvent.SurfaceResized -> "surfaceResized ${event.width}x${event.height}"
      ExplorerHostEvent.SurfaceDetached -> "surfaceDetached"
      is ExplorerHostEvent.Error ->
        "error code=${event.code} url=${event.url} message=${event.message}"
      else -> name
    }

  companion object {
    fun from(event: ExplorerHostEvent): NativeHostEvent = NativeHostEvent(event)
  }
}

data class NativeNavigationRequest(
  val navigationId: String,
  val url: String
)

data class NativeSimpleDialogRequest(
  val dialogId: String,
  val kind: String,
  val message: String,
  val defaultValue: String?
)

data class NativeInputMethodRequest(
  val inputMethodId: String,
  val type: String,
  val text: String,
  val insertionPoint: Int?,
  val multiline: Boolean,
  val allowVirtualKeyboard: Boolean
)

data class NativeSelectElementRequest(
  val selectElementId: String,
  val options: List<NativeSelectElementOptionOrOptgroup>,
  val selectedOptions: List<Int>,
  val allowSelectMultiple: Boolean
) {
  fun flatOptions(): List<NativeSelectElementOption> =
    options.flatMap { item ->
      when (item) {
        is NativeSelectElementOptionOrOptgroup.Option -> listOf(item.option)
        is NativeSelectElementOptionOrOptgroup.Optgroup -> item.options
      }
    }

  fun optionCount(): Int = flatOptions().size

  fun disabledOptionCount(): Int = flatOptions().count { it.isDisabled }

  fun optgroupLabels(): List<String> =
    options.mapNotNull { item ->
      (item as? NativeSelectElementOptionOrOptgroup.Optgroup)?.label
    }
}

data class NativeSelectElementOption(
  val id: Int,
  val label: String,
  val isDisabled: Boolean
)

sealed interface NativeSelectElementOptionOrOptgroup {
  data class Option(val option: NativeSelectElementOption) : NativeSelectElementOptionOrOptgroup

  data class Optgroup(
    val label: String,
    val options: List<NativeSelectElementOption>
  ) : NativeSelectElementOptionOrOptgroup
}

data class NativeContextMenuRequest(
  val contextMenuId: String,
  val x: Int,
  val y: Int,
  val width: Int,
  val height: Int,
  val elementInfo: NativeContextMenuElementInformation,
  val items: List<NativeContextMenuItem>
) {
  fun itemCount(): Int = items.count { it is NativeContextMenuItem.Item }

  fun enabledItemCount(): Int =
    items.count { item -> item is NativeContextMenuItem.Item && item.enabled && item.action.isNotBlank() }
}

data class NativeFilePickerRequest(
  val filePickerId: String,
  val currentPaths: List<String>,
  val filterPatterns: List<String>,
  val allowSelectMultiple: Boolean
)

data class NativePermissionRequest(
  val permission: String,
  val origin: String
)

data class NativeContextMenuElementInformation(
  val isLink: Boolean,
  val isImage: Boolean,
  val isEditableText: Boolean,
  val hasSelection: Boolean,
  val linkUrl: String?,
  val imageUrl: String?,
  val contextType: String
) {
  companion object {
    fun empty(): NativeContextMenuElementInformation =
      NativeContextMenuElementInformation(
        isLink = false,
        isImage = false,
        isEditableText = false,
        hasSelection = false,
        linkUrl = null,
        imageUrl = null,
        contextType = "page"
      )
  }
}

sealed interface NativeContextMenuItem {
  data class Item(
    val label: String,
    val action: String,
    val enabled: Boolean
  ) : NativeContextMenuItem

  data object Separator : NativeContextMenuItem
}

private fun ExplorerHostEvent.hostEventName(): String =
  when (this) {
    is ExplorerHostEvent.NavigationRequested -> "navigationRequested"
    is ExplorerHostEvent.PopupRequested -> "popupRequested"
    is ExplorerHostEvent.PopupCreated -> "popupCreated"
    is ExplorerHostEvent.PopupCreationFailed -> "popupCreationFailed"
    is ExplorerHostEvent.UrlChanged -> "urlChanged"
    is ExplorerHostEvent.PageTitleChanged -> "pageTitleChanged"
    is ExplorerHostEvent.StatusTextChanged -> "statusTextChanged"
    is ExplorerHostEvent.LoadStatusChanged -> "loadStatusChanged"
    is ExplorerHostEvent.HistoryChanged -> "historyChanged"
    ExplorerHostEvent.Closed -> "closed"
    is ExplorerHostEvent.Crashed -> "crashed"
    is ExplorerHostEvent.Error -> "error"
    is ExplorerHostEvent.JavaScriptEvaluationResult -> "javascriptEvaluationResult"
    is ExplorerHostEvent.SimpleDialogRequested -> "simpleDialogRequested"
    is ExplorerHostEvent.SimpleDialogDismissed -> "simpleDialogDismissed"
    is ExplorerHostEvent.InputMethodRequested -> "inputMethodRequested"
    is ExplorerHostEvent.InputMethodDismissed -> "inputMethodDismissed"
    is ExplorerHostEvent.SelectElementRequested -> "selectElementRequested"
    is ExplorerHostEvent.SelectElementDismissed -> "selectElementDismissed"
    is ExplorerHostEvent.ContextMenuRequested -> "contextMenuRequested"
    is ExplorerHostEvent.ContextMenuDismissed -> "contextMenuDismissed"
    is ExplorerHostEvent.FilePickerRequested -> "filePickerRequested"
    is ExplorerHostEvent.FilePickerDismissed -> "filePickerDismissed"
    is ExplorerHostEvent.PermissionRequested -> "permissionRequested"
    is ExplorerHostEvent.FocusChanged -> "focusChanged"
    is ExplorerHostEvent.CursorChanged -> "cursorChanged"
    is ExplorerHostEvent.FullscreenChanged -> "fullscreenChanged"
    is ExplorerHostEvent.SurfaceAttached -> "surfaceAttached"
    is ExplorerHostEvent.SurfaceResized -> "surfaceResized"
    ExplorerHostEvent.SurfaceDetached -> "surfaceDetached"
  }

private fun toNativeSelectElementOptionOrOptgroup(
  item: SelectElementOptionOrOptgroup
): NativeSelectElementOptionOrOptgroup =
  when (item) {
    is SelectElementOptionOrOptgroup.Option ->
      NativeSelectElementOptionOrOptgroup.Option(item.option.toNativeSelectElementOption())
    is SelectElementOptionOrOptgroup.Optgroup ->
      NativeSelectElementOptionOrOptgroup.Optgroup(
        label = item.label,
        options = item.options.map(SelectElementOption::toNativeSelectElementOption)
      )
  }

private fun SelectElementOption.toNativeSelectElementOption(): NativeSelectElementOption =
  NativeSelectElementOption(
    id = id,
    label = label,
    isDisabled = isDisabled
  )

private fun ContextMenuElementInformation.toNativeContextMenuElementInformation(): NativeContextMenuElementInformation =
  NativeContextMenuElementInformation(
    isLink = isLink,
    isImage = isImage,
    isEditableText = isEditableText,
    hasSelection = hasSelection,
    linkUrl = linkUrl,
    imageUrl = imageUrl,
    contextType = contextType
  )

private fun toNativeContextMenuItem(item: ContextMenuItem): NativeContextMenuItem =
  when (item) {
    is ContextMenuItem.Item ->
      NativeContextMenuItem.Item(
        label = item.label.ifBlank { item.action },
        action = item.action,
        enabled = item.enabled
      )
    ContextMenuItem.Separator -> NativeContextMenuItem.Separator
  }
