package com.kepler.explorerkit.androidhost

/**
 * Proof-only Kotlin bridge types for the reusable Android host module.
 *
 * These types are intentionally narrow and may change as the Android host
 * extraction continues; they are not a stable public Kotlin SDK.
 */
data class SelectElementOption(
  val id: Int,
  val label: String,
  val isDisabled: Boolean
)

sealed interface SelectElementOptionOrOptgroup {
  data class Option(val option: SelectElementOption) : SelectElementOptionOrOptgroup

  data class Optgroup(
    val label: String,
    val options: List<SelectElementOption>
  ) : SelectElementOptionOrOptgroup
}

sealed interface ContextMenuItem {
  data class Item(
    val label: String,
    val action: String,
    val enabled: Boolean
  ) : ContextMenuItem

  data object Separator : ContextMenuItem
}

data class ContextMenuElementInformation(
  val isLink: Boolean,
  val isImage: Boolean,
  val isEditableText: Boolean,
  val hasSelection: Boolean,
  val linkUrl: String?,
  val imageUrl: String?,
  val contextType: String
)

sealed interface ExplorerHostEvent {
  data class NavigationRequested(
    val navigationId: String,
    val url: String
  ) : ExplorerHostEvent

  data class PopupRequested(
    val requestId: String,
    val parentWebViewId: String,
    val parentUrl: String?,
    val targetUrl: String?,
    val windowFeatures: String?,
    val policy: String
  ) : ExplorerHostEvent

  data class PopupCreated(
    val requestId: String,
    val parentWebViewId: String,
    val childWebViewId: String,
    val parentUrl: String?,
    val targetUrl: String?,
    val windowFeatures: String?,
    val policy: String
  ) : ExplorerHostEvent

  data class PopupCreationFailed(
    val requestId: String,
    val errorMessage: String
  ) : ExplorerHostEvent

  data class UrlChanged(val url: String) : ExplorerHostEvent

  data class PageTitleChanged(val title: String?) : ExplorerHostEvent

  data class StatusTextChanged(val status: String?) : ExplorerHostEvent

  data class LoadStatusChanged(val status: String) : ExplorerHostEvent

  data class HistoryChanged(
    val entries: List<String>,
    val current: Int,
    val canGoBack: Boolean,
    val canGoForward: Boolean
  ) : ExplorerHostEvent

  data object Closed : ExplorerHostEvent

  data class Crashed(
    val url: String?,
    val reason: String,
    val backtrace: String?
  ) : ExplorerHostEvent

  data class Error(
    val url: String?,
    val code: Int,
    val message: String
  ) : ExplorerHostEvent

  data class JavaScriptEvaluationResult(
    val evaluationId: String,
    val ok: Boolean,
    val valueJson: String?,
    val errorType: String?
  ) : ExplorerHostEvent

  data class SimpleDialogRequested(
    val dialogId: String,
    val kind: String,
    val message: String,
    val defaultValue: String?
  ) : ExplorerHostEvent

  data class SimpleDialogDismissed(val dialogId: String) : ExplorerHostEvent

  data class InputMethodRequested(
    val inputMethodId: String,
    val type: String,
    val text: String,
    val insertionPoint: Int?,
    val multiline: Boolean,
    val allowVirtualKeyboard: Boolean
  ) : ExplorerHostEvent

  data class InputMethodDismissed(val inputMethodId: String) : ExplorerHostEvent

  data class SelectElementRequested(
    val selectElementId: String,
    val options: List<SelectElementOptionOrOptgroup>,
    val selectedOptions: List<Int>,
    val allowSelectMultiple: Boolean
  ) : ExplorerHostEvent

  data class SelectElementDismissed(val selectElementId: String) : ExplorerHostEvent

  data class ContextMenuRequested(
    val contextMenuId: String,
    val x: Int,
    val y: Int,
    val width: Int,
    val height: Int,
    val elementInfo: ContextMenuElementInformation,
    val items: List<ContextMenuItem>
  ) : ExplorerHostEvent

  data class ContextMenuDismissed(val contextMenuId: String) : ExplorerHostEvent

  data class FilePickerRequested(
    val filePickerId: String,
    val currentPaths: List<String>,
    val filterPatterns: List<String>,
    val allowSelectMultiple: Boolean
  ) : ExplorerHostEvent

  data class FilePickerDismissed(val filePickerId: String) : ExplorerHostEvent

  data class PermissionRequested(
    val permission: String,
    val origin: String
  ) : ExplorerHostEvent

  data class FocusChanged(val isFocused: Boolean) : ExplorerHostEvent

  data class CursorChanged(val cursor: String) : ExplorerHostEvent

  data class FullscreenChanged(val isFullscreen: Boolean) : ExplorerHostEvent

  data class SurfaceAttached(val width: Int, val height: Int) : ExplorerHostEvent

  data class SurfaceResized(val width: Int, val height: Int) : ExplorerHostEvent

  data object SurfaceDetached : ExplorerHostEvent
}
