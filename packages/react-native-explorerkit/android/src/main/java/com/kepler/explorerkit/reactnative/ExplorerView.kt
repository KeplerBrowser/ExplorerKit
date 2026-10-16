package com.kepler.explorerkit.reactnative

import com.kepler.explorerkit.androidhost.ContextMenuElementInformation
import com.kepler.explorerkit.androidhost.ContextMenuItem
import com.kepler.explorerkit.androidhost.IME_COMPOSITION_END
import com.kepler.explorerkit.androidhost.IME_COMPOSITION_UPDATE
import com.kepler.explorerkit.androidhost.KEYBOARD_KEY_BACKSPACE
import com.kepler.explorerkit.androidhost.KEYBOARD_KEY_DELETE
import com.kepler.explorerkit.androidhost.KEYBOARD_KEY_ENTER
import com.kepler.explorerkit.androidhost.SIMPLE_DIALOG_ACTION_CONFIRM
import com.kepler.explorerkit.androidhost.SIMPLE_DIALOG_ACTION_DISMISS
import com.kepler.explorerkit.androidhost.SelectElementOption
import com.kepler.explorerkit.androidhost.SelectElementOptionOrOptgroup
import com.kepler.explorerkit.androidhost.ExplorerColorValue
import com.kepler.explorerkit.androidhost.ExplorerHostEvent
import com.kepler.explorerkit.androidhost.ExplorerInputPickerValues
import com.kepler.explorerkit.androidhost.ExplorerSurfaceLifecycleCoordinator
import com.kepler.explorerkit.androidhost.ExplorerViewBinding
import com.kepler.explorerkit.androidhost.TOUCH_EVENT_CANCEL
import com.kepler.explorerkit.androidhost.TOUCH_EVENT_DOWN
import com.kepler.explorerkit.androidhost.TOUCH_EVENT_MOVE
import com.kepler.explorerkit.androidhost.TOUCH_EVENT_UP

import android.app.AlertDialog
import android.app.Activity
import android.app.DatePickerDialog
import android.app.TimePickerDialog
import android.content.ActivityNotFoundException
import android.content.ContentResolver
import android.content.Context
import android.content.Intent
import android.content.DialogInterface
import android.content.res.Resources
import android.graphics.Color
import android.graphics.Rect
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.provider.OpenableColumns
import android.text.Editable
import android.text.InputType
import android.text.Selection
import android.text.SpannableStringBuilder
import android.util.AttributeSet
import android.view.Choreographer
import android.view.GestureDetector
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.ViewGroup
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager
import android.widget.ArrayAdapter
import android.widget.CheckBox
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.RadioButton
import android.widget.ScrollView
import android.widget.SeekBar
import android.widget.TextView
import android.webkit.MimeTypeMap
import androidx.activity.OnBackPressedCallback
import androidx.activity.OnBackPressedDispatcherOwner
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import com.facebook.react.bridge.BaseActivityEventListener
import com.facebook.react.uimanager.ThemedReactContext
import com.facebook.react.uimanager.UIManagerHelper
import org.json.JSONObject
import java.io.File
import java.io.IOException
import java.time.LocalDate
import java.time.LocalDateTime
import java.time.LocalTime
import java.time.YearMonth
import java.util.Locale
import java.util.UUID
import kotlin.math.roundToInt

private const val FILE_PICKER_REQUEST_CODE = 0x5346
private const val STATUS_OK = 0
private const val STATUS_NULL_POINTER = 2
private const val STATUS_BACKEND_ERROR = 4
private const val STATUS_INVALID_CONTROLLER_HANDLE = 5
private const val STATUS_EXPIRED_CONTROLLER_HANDLE = 6

class ExplorerView : FrameLayout {
  private val binding = ExplorerViewBinding(context.applicationContext)
  private val mainHandler = Handler(Looper.getMainLooper())
  private val viewportInsetsController = ExplorerViewportInsetsController()
  private val surfaceLifecycleCoordinator = ExplorerSurfaceLifecycleCoordinator(
    onSurfaceAttached = { surface, width, height ->
      viewportInsetsController.onSurfaceAttached(width, height)?.let { viewport ->
        dispatchHostEvents(
          binding.attachSurface(surface, viewport.width, viewport.height, currentDensity())
        )
      }
    },
    onSurfaceResized = { width, height ->
      viewportInsetsController.onSurfaceResized(width, height)?.let { viewport ->
        dispatchHostEvents(binding.resizeSurface(viewport.width, viewport.height, currentDensity()))
      }
    },
    onSurfaceDetached = {
      viewportInsetsController.onSurfaceDetached()
      dispatchHostEvents(binding.detachSurface())
    },
    onLoadUrl = { url ->
      dispatchHostEvents(binding.loadUrl(url))
      scheduleFrameCallback()
    },
    onRenderingStarted = ::startFrameLoop,
    onRenderingStopped = ::stopFrameLoop
  )
  private val frameCallback =
    object : Choreographer.FrameCallback {
      override fun doFrame(frameTimeNanos: Long) {
        frameCallbackPosted = false
        if (disposed || !isFrameLoopActive) {
          return
        }

        dispatchHostEvents(binding.performUpdates())
        scheduleFrameCallback()
      }
    }
  private val gestureDetector =
    GestureDetector(
      context,
      object : GestureDetector.SimpleOnGestureListener() {
        override fun onDown(e: MotionEvent): Boolean = true

        override fun onLongPress(e: MotionEvent) {
          triggerContextMenu(e)
        }
      }
    )
  private val surfaceCallback =
    object : SurfaceHolder.Callback {
      override fun surfaceCreated(holder: SurfaceHolder) {
        val surfaceFrame = holder.surfaceFrame
        surfaceLifecycleCoordinator.onSurfaceCreated(
          holder.surface,
          surfaceFrame.width(),
          surfaceFrame.height()
        )
      }

      override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        surfaceLifecycleCoordinator.onSurfaceResized(width, height)
      }

      override fun surfaceDestroyed(holder: SurfaceHolder) {
        surfaceLifecycleCoordinator.onSurfaceDestroyed()
      }
    }
  private val surfaceView: SurfaceView
  private val activeDialogs = mutableMapOf<String, AlertDialog>()
  private val imeEditable = SpannableStringBuilder()
  private val pendingReactNativeDialogs = mutableSetOf<String>()
  private val pendingReactNativeContextMenus =
    mutableMapOf<String, ExplorerHostEvent.ContextMenuRequested>()
  private var disposed = false
  private var controllerReadyDispatched = false
  private var frameCallbackPosted = false
  private var isFrameLoopActive = false
  private var activeInputMethod: ExplorerHostEvent.InputMethodRequested? = null
  private var activeInputPickerDialog: AlertDialog? = null
  private var activeInputPickerId: String? = null
  private var activeSelectElement: ExplorerHostEvent.SelectElementRequested? = null
  private var activeSelectElementDialog: AlertDialog? = null
  private var activeContextMenu: ExplorerHostEvent.ContextMenuRequested? = null
  private var activeContextMenuDialog: AlertDialog? = null
  private var contextMenuPointerId: Int? = null
  private var activeFilePicker: ExplorerHostEvent.FilePickerRequested? = null
  private var activePermissionRequest: ExplorerHostEvent.PermissionRequested? = null
  private var activePermissionDialog: AlertDialog? = null
  private var filePickerListenerRegistered = false
  private val filePickerCacheRoot = File(context.cacheDir, "explorerkit-file-picker-${UUID.randomUUID()}")
  private val filePickerActivityListener: BaseActivityEventListener =
    object : BaseActivityEventListener() {
      override fun onActivityResult(
        activity: Activity,
        requestCode: Int,
        resultCode: Int,
        data: Intent?
      ) {
        if (requestCode != FILE_PICKER_REQUEST_CODE || disposed) {
          return
        }
        handleFilePickerResult(resultCode, data)
      }
    }
  private val backPressedCallback =
    object : OnBackPressedCallback(false) {
      override fun handleOnBackPressed() {
        when {
          activeInputMethod != null -> dismissInputMethod()
          canGoBackInHost && isFocused -> goBack()
          else -> {
            isEnabled = false
            registeredBackPressedDispatcherOwner?.onBackPressedDispatcher?.onBackPressed()
            updateBackPressedCallback()
          }
        }
      }
    }

  var currentUrl: String? = null
    private set

  var useReactNativeJavaScriptDialogs: Boolean = false

  var useReactNativeContextMenus: Boolean = false
  var useReactNativeOnShouldStartLoadWithRequest: Boolean = false
  private var canGoBackInHost = false
  private var backendHasFocus = false
  private var registeredBackPressedDispatcherOwner: OnBackPressedDispatcherOwner? = null

  init {
    isFocusable = true
    isFocusableInTouchMode = true
    ViewCompat.setOnApplyWindowInsetsListener(this) { _, insets ->
      handleWindowInsets(insets)
      insets
    }
    surfaceView = SurfaceView(context)
    surfaceView.holder.addCallback(surfaceCallback)
    surfaceView.isClickable = true
    surfaceView.isFocusable = false
    surfaceView.isFocusableInTouchMode = false
    surfaceView.setOnTouchListener { _, event -> handleTouchEvent(event) }
    addView(surfaceView, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.MATCH_PARENT))
  }

  constructor(context: Context) : super(context)
  constructor(context: Context, attrs: AttributeSet?) : super(context, attrs)
  constructor(context: Context, attrs: AttributeSet?, defStyleAttr: Int) : super(
    context,
    attrs,
    defStyleAttr
  )

  fun loadUrl(url: String) {
    surfaceLifecycleCoordinator.loadUrl(url)
  }

  fun reload() {
    dispatchHostEvents(binding.reload())
    scheduleFrameCallback()
  }

  fun goBack() {
    dispatchHostEvents(binding.goBack())
    scheduleFrameCallback()
  }

  fun goForward() {
    dispatchHostEvents(binding.goForward())
    scheduleFrameCallback()
  }

  fun focus() {
    requestFocus()
    syncBackendFocusState(true)
  }

  fun blur() {
    clearFocus()
    syncBackendFocusState(false)
  }

  fun sendControllerCommand(commandJson: String) {
    val evaluationId = evaluationIdForJavaScriptEvaluationCommand(commandJson)
    if (evaluationId != null && !isJavaScriptEvaluationDispatchReady()) {
      dispatchHostEvents(
        listOf(
          ExplorerHostEvent.JavaScriptEvaluationResult(
            evaluationId = evaluationId,
            ok = false,
            valueJson = null,
            errorType = "WebViewNotReady"
          )
        )
      )
      return
    }

    val status = binding.sendControllerCommand(commandJson)
    if (status != STATUS_OK) {
      if (evaluationId != null) {
        dispatchHostEvents(
          listOf(
            ExplorerHostEvent.JavaScriptEvaluationResult(
              evaluationId = evaluationId,
              ok = false,
              valueJson = null,
              errorType = javascriptEvaluationErrorTypeForControllerStatus(status)
            )
          )
        )
      }
      return
    }

    scheduleFrameCallback()
  }

  fun dispose() {
    if (disposed) {
      return
    }

    resolveAndDismissAllDialogs()
    dismissActiveInputPicker()
    dismissActiveSelectElementDialog()
    dismissActiveContextMenuDialog()
    denyPendingPermissionRequest()
    dismissActivePermissionDialog()
    clearActiveFilePicker()
    unregisterFilePickerActivityListener()
    filePickerCacheRoot.deleteRecursively()
    pendingReactNativeContextMenus.clear()
    if (activeInputMethod != null) {
      binding.dismissInputMethod()
      clearActiveInputMethodUi()
    }
    activeSelectElement = null
    activeContextMenu = null
    activePermissionRequest = null
    disposed = true
    unregisterBackPressedCallback()
    surfaceView.holder.removeCallback(surfaceCallback)
    surfaceLifecycleCoordinator.onViewDisposed()
    binding.dispose()
  }

  override fun onAttachedToWindow() {
    super.onAttachedToWindow()
    registerBackPressedCallback()
    registerFilePickerActivityListener()
    dispatchControllerReady()
    updateBackPressedCallback()
    ViewCompat.requestApplyInsets(this)
  }

  override fun onDetachedFromWindow() {
    unregisterBackPressedCallback()
    super.onDetachedFromWindow()
  }

  override fun onFocusChanged(gainFocus: Boolean, direction: Int, previouslyFocusedRect: Rect?) {
    super.onFocusChanged(gainFocus, direction, previouslyFocusedRect)
    syncBackendFocusState(gainFocus)
    updateBackPressedCallback()
  }

  private fun dispatchHostEvents(events: List<ExplorerHostEvent>) {
    val reactContext = context as? ThemedReactContext
    val eventDispatcher = reactContext?.let(UIManagerHelper::getEventDispatcher)
    val surfaceId = reactContext?.let(UIManagerHelper::getSurfaceId)
    if (eventDispatcher != null && surfaceId != null) {
      dispatchControllerReady(eventDispatcher, surfaceId)
    }

    for (event in events) {
      when (event) {
        is ExplorerHostEvent.NavigationRequested ->
          if (useReactNativeOnShouldStartLoadWithRequest && eventDispatcher != null && surfaceId != null) {
            eventDispatcher.dispatchEvent(
              ExplorerShouldStartLoadWithRequestRequestedEvent(
                surfaceId = surfaceId,
                viewId = id,
                navigationId = event.navigationId,
                url = event.url
              )
            )
          } else {
            sendControllerCommand(resolveNavigationRequestCommandJson(event.navigationId, true))
          }
        is ExplorerHostEvent.PopupRequested ->
          if (eventDispatcher != null && surfaceId != null) {
            eventDispatcher.dispatchEvent(
              ExplorerCreateNewWebViewRequestedEvent(
                surfaceId = surfaceId,
                viewId = id,
                payload = createNewWebViewRequestedEventPayload(event)
              )
            )
          }
        is ExplorerHostEvent.PopupCreated,
        is ExplorerHostEvent.PopupCreationFailed -> Unit
        is ExplorerHostEvent.UrlChanged -> {
          currentUrl = event.url
          if (eventDispatcher != null && surfaceId != null) {
            eventDispatcher.dispatchEvent(ExplorerUrlChangedEvent(surfaceId, id, event.url))
          }
        }
        is ExplorerHostEvent.PageTitleChanged ->
          if (eventDispatcher != null && surfaceId != null) {
            eventDispatcher.dispatchEvent(ExplorerPageTitleChangedEvent(surfaceId, id, event.title))
          }
        is ExplorerHostEvent.StatusTextChanged ->
          if (eventDispatcher != null && surfaceId != null) {
            eventDispatcher.dispatchEvent(ExplorerStatusTextChangedEvent(surfaceId, id, event.status))
          }
        is ExplorerHostEvent.LoadStatusChanged -> {
          if (eventDispatcher != null && surfaceId != null) {
            eventDispatcher.dispatchEvent(
              ExplorerLoadStatusChangedEvent(surfaceId, id, event.status)
            )
          }
        }
        is ExplorerHostEvent.HistoryChanged -> {
          canGoBackInHost = event.canGoBack
          updateBackPressedCallback()
          if (eventDispatcher != null && surfaceId != null) {
            eventDispatcher.dispatchEvent(
              ExplorerHistoryChangedEvent(
                surfaceId = surfaceId,
                viewId = id,
                entries = event.entries,
                current = event.current,
                canGoBack = event.canGoBack,
                canGoForward = event.canGoForward
              )
            )
          }
        }
        is ExplorerHostEvent.FocusChanged ->
          if (eventDispatcher != null && surfaceId != null) {
            eventDispatcher.dispatchEvent(ExplorerFocusChangedEvent(surfaceId, id, event.isFocused))
          }
        is ExplorerHostEvent.CursorChanged ->
          if (eventDispatcher != null && surfaceId != null) {
            eventDispatcher.dispatchEvent(ExplorerCursorChangedEvent(surfaceId, id, event.cursor))
          }
        is ExplorerHostEvent.FullscreenChanged ->
          if (eventDispatcher != null && surfaceId != null) {
            eventDispatcher.dispatchEvent(
              ExplorerFullscreenChangedEvent(surfaceId, id, event.isFullscreen)
            )
          }
        ExplorerHostEvent.Closed ->
          if (eventDispatcher != null && surfaceId != null) {
            eventDispatcher.dispatchEvent(ExplorerClosedEvent(surfaceId, id))
          }
        is ExplorerHostEvent.Crashed -> {
          if (eventDispatcher != null && surfaceId != null) {
            eventDispatcher.dispatchEvent(
              ExplorerCrashedEvent(surfaceId, id, event.reason, event.backtrace)
            )
          }
        }
        is ExplorerHostEvent.Error -> {
          if (eventDispatcher != null && surfaceId != null) {
            eventDispatcher.dispatchEvent(
              ExplorerErrorEvent(surfaceId, id, event.code, event.message)
            )
          }
        }
        is ExplorerHostEvent.JavaScriptEvaluationResult ->
          if (eventDispatcher != null && surfaceId != null) {
            eventDispatcher.dispatchEvent(
              ExplorerJavaScriptEvaluationResultEvent(
                surfaceId = surfaceId,
                viewId = id,
                evaluationId = event.evaluationId,
                ok = event.ok,
                valueJson = event.valueJson,
                errorType = event.errorType
              )
            )
          }
        is ExplorerHostEvent.SimpleDialogRequested ->
          if (useReactNativeJavaScriptDialogs && eventDispatcher != null && surfaceId != null) {
            pendingReactNativeDialogs += event.dialogId
            eventDispatcher.dispatchEvent(
              ExplorerJavaScriptDialogRequestedEvent(
                surfaceId = surfaceId,
                viewId = id,
                dialogId = event.dialogId,
                kind = event.kind,
                message = event.message,
                defaultValue = event.defaultValue
              )
            )
          } else {
            showSimpleDialog(event)
          }
        is ExplorerHostEvent.SimpleDialogDismissed ->
          if (pendingReactNativeDialogs.remove(event.dialogId) &&
            eventDispatcher != null &&
            surfaceId != null
          ) {
            eventDispatcher.dispatchEvent(
              ExplorerJavaScriptDialogDismissedEvent(surfaceId, id, event.dialogId)
            )
          } else {
            dismissDialog(event.dialogId)
          }
        is ExplorerHostEvent.InputMethodRequested -> handleInputMethodRequested(event)
        is ExplorerHostEvent.InputMethodDismissed -> handleInputMethodDismissed(event)
        is ExplorerHostEvent.SelectElementRequested -> handleSelectElementRequested(event)
        is ExplorerHostEvent.SelectElementDismissed -> handleSelectElementDismissed(event)
        is ExplorerHostEvent.ContextMenuRequested ->
          if (useReactNativeContextMenus && eventDispatcher != null && surfaceId != null) {
            pendingReactNativeContextMenus[event.contextMenuId] = event
            eventDispatcher.dispatchEvent(
              ExplorerContextMenuRequestedEvent(
                surfaceId = surfaceId,
                viewId = id,
                contextMenuId = event.contextMenuId,
                element = event.elementInfo,
                servoItems = event.items.map(::toReactNativeServoContextMenuItem)
              )
            )
          } else {
            handleContextMenuRequested(event)
          }
        is ExplorerHostEvent.ContextMenuDismissed -> {
          if (activeContextMenu?.contextMenuId != event.contextMenuId) {
            pendingReactNativeContextMenus.remove(event.contextMenuId)
          }
          handleContextMenuDismissed(event)
        }
        is ExplorerHostEvent.FilePickerRequested -> handleFilePickerRequested(event)
        is ExplorerHostEvent.FilePickerDismissed -> handleFilePickerDismissed(event)
        is ExplorerHostEvent.PermissionRequested -> handlePermissionRequested(event)
        is ExplorerHostEvent.SurfaceAttached,
        is ExplorerHostEvent.SurfaceResized,
        ExplorerHostEvent.SurfaceDetached -> Unit
      }
    }
  }

  private fun handleTouchEvent(event: MotionEvent): Boolean {
    if (disposed) {
      return false
    }

    gestureDetector.onTouchEvent(event)

    when (event.actionMasked) {
      MotionEvent.ACTION_DOWN,
      MotionEvent.ACTION_POINTER_DOWN -> {
        if (event.actionMasked == MotionEvent.ACTION_DOWN) {
          contextMenuPointerId = null
          if (!isFocused) {
            requestFocus()
          }
        }
        dispatchTouchEvent(TOUCH_EVENT_DOWN, event.actionIndex, event)
      }
      MotionEvent.ACTION_MOVE -> {
        for (index in 0 until event.pointerCount) {
          if (event.getPointerId(index) == contextMenuPointerId) {
            continue
          }
          dispatchTouchEvent(TOUCH_EVENT_MOVE, index, event)
        }
      }
      MotionEvent.ACTION_UP,
      MotionEvent.ACTION_POINTER_UP -> {
        val pointerId = event.getPointerId(event.actionIndex)
        if (pointerId == contextMenuPointerId) {
          contextMenuPointerId = null
          return true
        }
        dispatchTouchEvent(TOUCH_EVENT_UP, event.actionIndex, event)
        if (event.actionMasked == MotionEvent.ACTION_UP) {
          performClick()
        }
      }
      MotionEvent.ACTION_CANCEL -> {
        for (index in 0 until event.pointerCount) {
          if (event.getPointerId(index) == contextMenuPointerId) {
            continue
          }
          dispatchTouchEvent(TOUCH_EVENT_CANCEL, index, event)
        }
        contextMenuPointerId = null
      }
      else -> return false
    }

    scheduleFrameCallback()
    return true
  }

  private fun dispatchTouchEvent(action: Int, pointerIndex: Int, event: MotionEvent) {
    dispatchHostEvents(
      binding.dispatchTouchEvent(
        action,
        event.getPointerId(pointerIndex),
        event.getX(pointerIndex),
        event.getY(pointerIndex)
      )
    )
  }

  private fun triggerContextMenu(event: MotionEvent) {
    if (event.pointerCount != 1) {
      return
    }

    val pointerId = event.getPointerId(0)
    if (contextMenuPointerId == pointerId) {
      return
    }

    contextMenuPointerId = pointerId
    dispatchHostEvents(binding.dispatchTouchEvent(TOUCH_EVENT_CANCEL, pointerId, event.x, event.y))
    dispatchHostEvents(binding.triggerContextMenu(event.x, event.y))
    scheduleFrameCallback()
  }

  override fun performClick(): Boolean = super.performClick()

  override fun onCheckIsTextEditor(): Boolean {
    val inputMethod = activeInputMethod ?: return false
    return !ExplorerInputPickerValues.isPickerInputType(inputMethod.type)
  }

  override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection? {
    val inputMethod = activeInputMethod ?: return null
    if (ExplorerInputPickerValues.isPickerInputType(inputMethod.type)) {
      return null
    }
    outAttrs.inputType = editorInputType(inputMethod)
    outAttrs.imeOptions =
      if (inputMethod.multiline) {
        EditorInfo.IME_FLAG_NO_FULLSCREEN or EditorInfo.IME_ACTION_NONE
      } else {
        EditorInfo.IME_FLAG_NO_FULLSCREEN or EditorInfo.IME_ACTION_DONE
      }
    val selection = inputMethod.insertionPoint ?: inputMethod.text.length
    outAttrs.initialSelStart = selection
    outAttrs.initialSelEnd = selection

    return object : BaseInputConnection(this, true) {
      override fun getEditable(): Editable = imeEditable

      override fun commitText(text: CharSequence?, newCursorPosition: Int): Boolean {
        val committedText = text?.toString().orEmpty()
        val handled = super.commitText(text, newCursorPosition)
        dispatchImeComposition(IME_COMPOSITION_END, committedText)
        return handled
      }

      override fun setComposingText(text: CharSequence?, newCursorPosition: Int): Boolean {
        val composingText = text?.toString().orEmpty()
        val handled = super.setComposingText(text, newCursorPosition)
        dispatchImeComposition(IME_COMPOSITION_UPDATE, composingText)
        return handled
      }

      override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
        val handled = super.deleteSurroundingText(beforeLength, afterLength)
        repeat(beforeLength.coerceAtLeast(0)) {
          dispatchKeyboardKey(KEYBOARD_KEY_BACKSPACE)
        }
        repeat(afterLength.coerceAtLeast(0)) {
          dispatchKeyboardKey(KEYBOARD_KEY_DELETE)
        }
        return handled
      }

      override fun sendKeyEvent(event: KeyEvent): Boolean {
        if (event.action != KeyEvent.ACTION_DOWN) {
          return super.sendKeyEvent(event)
        }

        when (event.keyCode) {
          KeyEvent.KEYCODE_DEL -> {
            dispatchKeyboardKey(KEYBOARD_KEY_BACKSPACE)
            return true
          }
          KeyEvent.KEYCODE_FORWARD_DEL -> {
            dispatchKeyboardKey(KEYBOARD_KEY_DELETE)
            return true
          }
          KeyEvent.KEYCODE_ENTER -> {
            dispatchKeyboardKey(KEYBOARD_KEY_ENTER)
            return true
          }
        }

        return super.sendKeyEvent(event)
      }

      override fun performEditorAction(actionCode: Int): Boolean {
        if (!inputMethod.multiline) {
          dispatchKeyboardKey(KEYBOARD_KEY_ENTER)
          return true
        }
        return super.performEditorAction(actionCode)
      }

      override fun setSelection(start: Int, end: Int): Boolean {
        Selection.setSelection(
          imeEditable,
          start.coerceIn(0, imeEditable.length),
          end.coerceIn(0, imeEditable.length)
        )
        return true
      }
    }
  }

  override fun onKeyPreIme(keyCode: Int, event: KeyEvent): Boolean {
    if (
      keyCode == KeyEvent.KEYCODE_BACK &&
      event.action == KeyEvent.ACTION_UP &&
      activeInputMethod != null
    ) {
      dismissInputMethod()
      // Consume hardware-back here because older Android versions can also dispatch the same
      // back action through OnBackPressedDispatcher while the IME is visible.
      return true
    }
    return super.onKeyPreIme(keyCode, event)
  }

  private fun currentDensity(): Float = resources.displayMetrics.density

  private fun handleWindowInsets(insets: WindowInsetsCompat) {
    val imeBottomInset =
      if (insets.isVisible(WindowInsetsCompat.Type.ime())) {
        insets.getInsets(WindowInsetsCompat.Type.ime()).bottom
      } else {
        0
      }
    post {
      if (disposed) {
        return@post
      }

      viewportInsetsController.onImeInsetChanged(imeBottomInset)?.let { viewport ->
        dispatchHostEvents(binding.resizeSurface(viewport.width, viewport.height, currentDensity()))
        scheduleFrameCallback()
      }
    }
  }

  private fun dialogHostContext(): Context =
    (context as? ThemedReactContext)?.currentActivity ?: context

  private fun handleInputMethodRequested(event: ExplorerHostEvent.InputMethodRequested) {
    activeInputMethod = event
    updateBackPressedCallback()
    if (ExplorerInputPickerValues.isPickerInputType(event.type)) {
      if (!isFocused) {
        requestFocus()
      }
      inputMethodManager()?.hideSoftInputFromWindow(windowToken, 0)
      inputMethodManager()?.restartInput(this)
      if (activeInputPickerId != event.inputMethodId || activeInputPickerDialog == null) {
        dismissActiveInputPicker()
        showInputPicker(event)
      }
      return
    }

    dismissActiveInputPicker()
    syncEditableFromInputMethod(event)
    if (!isFocused) {
      requestFocus()
    }
    inputMethodManager()?.restartInput(this)
    if (event.allowVirtualKeyboard) {
      post {
        inputMethodManager()?.showSoftInput(this, InputMethodManager.SHOW_IMPLICIT)
        ViewCompat.requestApplyInsets(this)
      }
    }
  }

  private fun handleInputMethodDismissed(event: ExplorerHostEvent.InputMethodDismissed) {
    if (activeInputMethod?.inputMethodId != event.inputMethodId) {
      return
    }

    clearActiveInputMethodUi()
    updateBackPressedCallback()
    ViewCompat.requestApplyInsets(this)
  }

  private fun syncEditableFromInputMethod(event: ExplorerHostEvent.InputMethodRequested) {
    imeEditable.replace(0, imeEditable.length, event.text)
    val selection = (event.insertionPoint ?: event.text.length).coerceIn(0, imeEditable.length)
    Selection.setSelection(imeEditable, selection)
  }

  private fun editorInputType(inputMethod: ExplorerHostEvent.InputMethodRequested): Int {
    var inputType =
      when (inputMethod.type) {
        "email" -> InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS
        "number" -> InputType.TYPE_CLASS_NUMBER
        "password" -> InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD
        "search" -> InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_WEB_EDIT_TEXT
        "tel" -> InputType.TYPE_CLASS_PHONE
        "text" -> InputType.TYPE_CLASS_TEXT
        "url" -> InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI
        else -> InputType.TYPE_CLASS_TEXT
      }
    if (inputMethod.multiline) {
      inputType = inputType or InputType.TYPE_TEXT_FLAG_MULTI_LINE
    }
    return inputType
  }

  private fun inputMethodManager(): InputMethodManager? =
    context.getSystemService(Context.INPUT_METHOD_SERVICE) as? InputMethodManager

  private fun showInputPicker(event: ExplorerHostEvent.InputMethodRequested) {
    when (event.type) {
      "color" -> showColorPicker(event)
      "date" -> showDatePicker(event)
      "datetime-local" -> showDateTimeLocalPicker(event)
      "month" -> showMonthPicker(event)
      "time" -> showTimePicker(event)
      "week" -> showWeekPicker(event)
      else -> Unit
    }
  }

  private fun showColorPicker(event: ExplorerHostEvent.InputMethodRequested) {
    var selectedColor = ExplorerInputPickerValues.initialColorValue(event.text)
    val padding = dp(16)
    val preview =
      View(context).apply {
        layoutParams = LinearLayout.LayoutParams(
          LayoutParams.MATCH_PARENT,
          dp(48)
        )
        setBackgroundColor(Color.rgb(selectedColor.red, selectedColor.green, selectedColor.blue))
      }
    val valueLabel =
      TextView(context).apply {
        text = selectedColor.hex()
        setPadding(0, dp(12), 0, dp(8))
      }
    val container =
      LinearLayout(context).apply {
        orientation = LinearLayout.VERTICAL
        setPadding(padding, padding, padding, 0)
        addView(preview)
        addView(valueLabel)
      }
    fun updateSelectedColor(color: ExplorerColorValue) {
      selectedColor = color
      valueLabel.text = color.hex()
      preview.setBackgroundColor(Color.rgb(color.red, color.green, color.blue))
    }
    container.addView(createColorSlider("Red", selectedColor.red) { value ->
      updateSelectedColor(selectedColor.copy(red = value))
    })
    container.addView(createColorSlider("Green", selectedColor.green) { value ->
      updateSelectedColor(selectedColor.copy(green = value))
    })
    container.addView(createColorSlider("Blue", selectedColor.blue) { value ->
      updateSelectedColor(selectedColor.copy(blue = value))
    })

    val dialog =
      AlertDialog.Builder(dialogHostContext())
        .setTitle("Pick a color")
        .setView(container)
        .setPositiveButton(android.R.string.ok) { _, _ ->
          commitInputMethodValue(selectedColor.hex())
          dismissInputMethod()
        }
        .setNegativeButton(android.R.string.cancel) { _, _ ->
          dismissInputMethod()
        }
        .create()
    dialog.setOnCancelListener {
      dismissInputMethod()
    }
    showInputPickerDialog(event.inputMethodId, dialog)
  }

  private fun createColorSlider(
    label: String,
    initialValue: Int,
    onChanged: (Int) -> Unit
  ): View =
    LinearLayout(context).apply {
      orientation = LinearLayout.VERTICAL
      addView(
        TextView(context).apply {
          text = "$label: $initialValue"
        }
      )
      val sliderLabel = getChildAt(0) as TextView
      addView(
        SeekBar(context).apply {
          max = 255
          progress = initialValue
          setOnSeekBarChangeListener(
            object : SeekBar.OnSeekBarChangeListener {
              override fun onProgressChanged(seekBar: SeekBar?, progress: Int, fromUser: Boolean) {
                sliderLabel.text = "$label: $progress"
                onChanged(progress)
              }

              override fun onStartTrackingTouch(seekBar: SeekBar?) = Unit

              override fun onStopTrackingTouch(seekBar: SeekBar?) = Unit
            }
          )
        }
      )
    }

  private fun showDatePicker(event: ExplorerHostEvent.InputMethodRequested) {
    val initialDate = ExplorerInputPickerValues.initialDate(event.text, LocalDate.now())
    val dialog =
      createDatePickerDialog("Pick a date", initialDate) { selectedDate ->
        commitInputMethodValue(ExplorerInputPickerValues.formatDate(selectedDate))
        dismissInputMethod()
      }
    showInputPickerDialog(event.inputMethodId, dialog)
  }

  private fun showDateTimeLocalPicker(event: ExplorerHostEvent.InputMethodRequested) {
    val initialDateTime = ExplorerInputPickerValues.initialDateTimeLocal(event.text, LocalDateTime.now())
    val dateDialog =
      createDatePickerDialog("Pick a date", initialDateTime.toLocalDate()) { selectedDate ->
        post {
          if (disposed || activeInputMethod?.inputMethodId != event.inputMethodId) {
            return@post
          }
          val timeDialog =
            createTimePickerDialog("Pick a time", initialDateTime.toLocalTime()) { selectedTime ->
              commitInputMethodValue(
                ExplorerInputPickerValues.formatDateTimeLocal(LocalDateTime.of(selectedDate, selectedTime))
              )
              dismissInputMethod()
            }
          showInputPickerDialog(event.inputMethodId, timeDialog)
        }
      }
    showInputPickerDialog(event.inputMethodId, dateDialog)
  }

  private fun showMonthPicker(event: ExplorerHostEvent.InputMethodRequested) {
    val initialMonth = ExplorerInputPickerValues.initialMonth(event.text, YearMonth.now())
    val dialog =
      createDatePickerDialog(
        title = "Pick a month",
        initialDate = initialMonth.atDay(1),
        hideDay = true
      ) { selectedDate ->
        commitInputMethodValue(
          ExplorerInputPickerValues.formatMonth(YearMonth.of(selectedDate.year, selectedDate.month))
        )
        dismissInputMethod()
      }
    showInputPickerDialog(event.inputMethodId, dialog)
  }

  private fun showTimePicker(event: ExplorerHostEvent.InputMethodRequested) {
    val initialTime = ExplorerInputPickerValues.initialTime(event.text, LocalTime.now())
    val dialog =
      createTimePickerDialog("Pick a time", initialTime) { selectedTime ->
        commitInputMethodValue(ExplorerInputPickerValues.formatTime(selectedTime))
        dismissInputMethod()
      }
    showInputPickerDialog(event.inputMethodId, dialog)
  }

  private fun showWeekPicker(event: ExplorerHostEvent.InputMethodRequested) {
    val initialDate = ExplorerInputPickerValues.initialWeekDate(event.text, LocalDate.now())
    val dialog =
      createDatePickerDialog("Pick a week", initialDate) { selectedDate ->
        commitInputMethodValue(ExplorerInputPickerValues.formatWeek(selectedDate))
        dismissInputMethod()
      }
    showInputPickerDialog(event.inputMethodId, dialog)
  }

  private fun createDatePickerDialog(
    title: String,
    initialDate: LocalDate,
    hideDay: Boolean = false,
    onDatePicked: (LocalDate) -> Unit
  ): DatePickerDialog =
    DatePickerDialog(
      dialogHostContext(),
      { _, year, monthOfYear, dayOfMonth ->
        onDatePicked(LocalDate.of(year, monthOfYear + 1, dayOfMonth))
      },
      initialDate.year,
      initialDate.monthValue - 1,
      initialDate.dayOfMonth
    ).apply {
      setTitle(title)
      setButton(DialogInterface.BUTTON_NEGATIVE, context.getString(android.R.string.cancel)) { _, _ ->
        dismissInputMethod()
      }
      setOnCancelListener {
        dismissInputMethod()
      }
      if (hideDay) {
        val dayFieldId = Resources.getSystem().getIdentifier("day", "id", "android")
        datePicker.findViewById<View>(dayFieldId)?.visibility = View.GONE
      }
    }

  private fun createTimePickerDialog(
    title: String,
    initialTime: LocalTime,
    onTimePicked: (LocalTime) -> Unit
  ): TimePickerDialog =
    TimePickerDialog(
      dialogHostContext(),
      { _, hourOfDay, minute ->
        onTimePicked(LocalTime.of(hourOfDay, minute))
      },
      initialTime.hour,
      initialTime.minute,
      true
    ).apply {
      setTitle(title)
      setButton(DialogInterface.BUTTON_NEGATIVE, context.getString(android.R.string.cancel)) { _, _ ->
        dismissInputMethod()
      }
      setOnCancelListener {
        dismissInputMethod()
      }
    }

  private fun showInputPickerDialog(inputMethodId: String, dialog: AlertDialog) {
    activeInputPickerId = inputMethodId
    activeInputPickerDialog = dialog
    dialog.setOnDismissListener {
      if (activeInputPickerDialog === dialog) {
        activeInputPickerDialog = null
        activeInputPickerId = null
      }
    }
    dialog.show()
  }

  private fun dismissActiveInputPicker() {
    val dialog = activeInputPickerDialog ?: return
    activeInputPickerDialog = null
    activeInputPickerId = null
    dialog.setOnDismissListener(null)
    dialog.setOnCancelListener(null)
    dialog.dismiss()
  }

  private fun clearActiveInputMethodUi() {
    dismissActiveInputPicker()
    activeInputMethod = null
    imeEditable.clear()
    inputMethodManager()?.hideSoftInputFromWindow(windowToken, 0)
    inputMethodManager()?.restartInput(this)
    updateBackPressedCallback()
  }

  private fun syncBackendFocusState(focused: Boolean) {
    if (disposed || backendHasFocus == focused) {
      return
    }

    backendHasFocus = focused
    dispatchHostEvents(if (focused) binding.focus() else binding.blur())
    scheduleFrameCallback()
  }

  private fun backPressedDispatcherOwner(): OnBackPressedDispatcherOwner? =
    ((context as? ThemedReactContext)?.currentActivity as? OnBackPressedDispatcherOwner)
      ?: (context as? OnBackPressedDispatcherOwner)

  private fun registerBackPressedCallback() {
    val owner = backPressedDispatcherOwner() ?: return
    backPressedCallback.remove()
    registeredBackPressedDispatcherOwner = owner
    owner.onBackPressedDispatcher.addCallback(owner, backPressedCallback)
  }

  private fun unregisterBackPressedCallback() {
    backPressedCallback.remove()
    registeredBackPressedDispatcherOwner = null
  }

  private fun updateBackPressedCallback() {
    backPressedCallback.isEnabled =
      !disposed && (activeInputMethod != null || (canGoBackInHost && isFocused))
  }

  private fun handleSelectElementRequested(event: ExplorerHostEvent.SelectElementRequested) {
    if (activeSelectElement?.selectElementId == event.selectElementId && activeSelectElementDialog != null) {
      return
    }

    dismissActiveSelectElementDialog()
    activeSelectElement = event
    showSelectElementPicker(event)
  }

  private fun handleSelectElementDismissed(event: ExplorerHostEvent.SelectElementDismissed) {
    if (activeSelectElement?.selectElementId != event.selectElementId) {
      return
    }

    dismissActiveSelectElementDialog()
    activeSelectElement = null
  }

  private fun showSelectElementPicker(event: ExplorerHostEvent.SelectElementRequested) {
    val optionButtons = mutableListOf<Pair<Int, TextView>>()
    val initialSelectedOptionId = event.selectedOptions.firstOrNull()
    val initialSelectedOptions = event.selectedOptions.toSet()
    val content =
      LinearLayout(context).apply {
        orientation = LinearLayout.VERTICAL
        setPadding(dp(16), dp(8), dp(16), 0)
      }

    fun addOption(option: SelectElementOption, nested: Boolean) {
      val button =
        if (event.allowSelectMultiple) {
          CheckBox(context)
        } else {
          RadioButton(context)
        }.apply {
          text = option.label
          isEnabled = !option.isDisabled
          isChecked =
            if (event.allowSelectMultiple) {
              initialSelectedOptions.contains(option.id)
            } else {
              option.id == initialSelectedOptionId
            }
          if (nested) {
            setPadding(dp(12), paddingTop, paddingRight, paddingBottom)
          }
        }
      if (!event.allowSelectMultiple) {
        button.setOnClickListener {
          optionButtons.forEach { (_, candidate) ->
            if (candidate !== button) {
              (candidate as? RadioButton)?.isChecked = false
            }
          }
          (button as RadioButton).isChecked = true
        }
      }
      optionButtons += option.id to button
      content.addView(button)
    }

    for (item in event.options) {
      when (item) {
        is SelectElementOptionOrOptgroup.Option -> addOption(item.option, nested = false)
        is SelectElementOptionOrOptgroup.Optgroup -> {
          content.addView(
            TextView(context).apply {
              text = item.label
              setPadding(0, dp(8), 0, dp(4))
            }
          )
          item.options.forEach { option -> addOption(option, nested = true) }
        }
      }
    }

    val scrollView =
      ScrollView(context).apply {
        addView(
          content,
          LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.WRAP_CONTENT)
        )
      }
    var resolved = false
    fun currentSelection(): IntArray =
      optionButtons
        .filter { (_, button) ->
          when (button) {
            is CheckBox -> button.isChecked
            is RadioButton -> button.isChecked
            else -> false
          }
        }
        .map { (id, _) -> id }
        .toIntArray()
    fun resolveSelection(selectedOptions: IntArray) {
      if (resolved || activeSelectElement?.selectElementId != event.selectElementId) {
        return
      }

      resolved = true
      activeSelectElement = null
      dispatchHostEvents(binding.resolveSelectElement(event.selectElementId, selectedOptions))
      scheduleFrameCallback()
    }

    val dialog =
      AlertDialog.Builder(dialogHostContext())
        .setTitle(if (event.allowSelectMultiple) "Select options" else "Select an option")
        .setView(scrollView)
        .setPositiveButton(android.R.string.ok) { _, _ ->
          resolveSelection(currentSelection())
        }
        .setNegativeButton(android.R.string.cancel) { _, _ ->
          resolveSelection(event.selectedOptions.toIntArray())
        }
        .create()
    activeSelectElementDialog = dialog
    dialog.setOnDismissListener {
      if (activeSelectElementDialog === dialog) {
        activeSelectElementDialog = null
      }
      if (!resolved) {
        resolveSelection(event.selectedOptions.toIntArray())
      }
    }
    dialog.show()
  }

  private fun dismissActiveSelectElementDialog() {
    val dialog = activeSelectElementDialog ?: return
    activeSelectElementDialog = null
    dialog.setOnDismissListener(null)
    dialog.dismiss()
  }

  private fun toReactNativeServoContextMenuItem(item: ContextMenuItem): ReactNativeContextMenuItem =
    when (item) {
      is ContextMenuItem.Item ->
        ReactNativeContextMenuItem(
          kind = ContextMenuItemKind.ITEM,
          source = ContextMenuItemSource.SERVO,
          label = item.label,
          action = item.action,
          enabled = item.enabled
        )
      ContextMenuItem.Separator ->
        ReactNativeContextMenuItem(
          kind = ContextMenuItemKind.SEPARATOR,
          source = ContextMenuItemSource.SERVO
        )
    }

  internal fun showReactNativeContextMenu(
    contextMenuId: String,
    injectedItems: List<ReactNativeContextMenuItem>
  ) {
    val event = pendingReactNativeContextMenus.remove(contextMenuId) ?: return
    if (activeContextMenu?.contextMenuId == event.contextMenuId && activeContextMenuDialog != null) {
      return
    }

    dismissActiveContextMenuDialog()
    activeContextMenu = event
    showContextMenu(event, injectedItems)
  }

  private fun handleContextMenuRequested(event: ExplorerHostEvent.ContextMenuRequested) {
    if (activeContextMenu?.contextMenuId == event.contextMenuId && activeContextMenuDialog != null) {
      return
    }

    dismissActiveContextMenuDialog()
    activeContextMenu = event
    showContextMenu(event, emptyList())
  }

  private fun handleContextMenuDismissed(event: ExplorerHostEvent.ContextMenuDismissed) {
    if (activeContextMenu?.contextMenuId != event.contextMenuId) {
      pendingReactNativeContextMenus.remove(event.contextMenuId)
      return
    }

    dismissActiveContextMenuDialog()
    activeContextMenu = null
  }

  private fun showContextMenu(
    event: ExplorerHostEvent.ContextMenuRequested,
    injectedItems: List<ReactNativeContextMenuItem>
  ) {
    data class ContextMenuDialogItem(
      val title: String,
      val subtitle: String?,
      val action: String?,
      val enabled: Boolean,
      val source: ContextMenuItemSource,
      val separator: Boolean = false,
      val header: Boolean = false
    )

    fun itemTitle(source: ContextMenuItemSource): String =
      when (source) {
        ContextMenuItemSource.SERVO -> "Servo actions"
        ContextMenuItemSource.APP -> "App actions"
      }

    fun itemSubtitle(source: ContextMenuItemSource): String =
      when (source) {
        ContextMenuItemSource.SERVO -> "Provided by Servo"
        ContextMenuItemSource.APP -> "Injected by React Native"
      }

    fun normalizeItems(items: List<ReactNativeContextMenuItem>): List<ReactNativeContextMenuItem> {
      val normalized = mutableListOf<ReactNativeContextMenuItem>()
      var previousWasSeparator = true
      for (item in items) {
        if (item.kind == ContextMenuItemKind.SEPARATOR) {
          if (!previousWasSeparator) {
            normalized += item
            previousWasSeparator = true
          }
          continue
        }

        normalized += item
        previousWasSeparator = false
      }
      while (normalized.lastOrNull()?.kind == ContextMenuItemKind.SEPARATOR) {
        normalized.removeAt(normalized.lastIndex)
      }
      return normalized
    }

    val seenActions = mutableSetOf<String>()
    fun toDialogItems(
      items: List<ReactNativeContextMenuItem>,
      source: ContextMenuItemSource
    ): List<ContextMenuDialogItem> =
      normalizeItems(items).mapNotNull { item ->
        when (item.kind) {
          ContextMenuItemKind.SEPARATOR ->
            ContextMenuDialogItem(
              title = "────────",
              subtitle = null,
              action = null,
              enabled = false,
              source = source,
              separator = true
            )
          ContextMenuItemKind.ITEM -> {
            val action = item.action ?: return@mapNotNull null
            if (!seenActions.add(action)) {
              return@mapNotNull null
            }
            ContextMenuDialogItem(
              title = item.label ?: action,
              subtitle = itemSubtitle(source),
              action = action,
              enabled = item.enabled,
              source = source
            )
          }
        }
      }

    fun contextLabel(type: String): String =
      when (type) {
        "link" -> "Link"
        "image" -> "Image"
        "media" -> "Media"
        "input" -> "Input"
        "text" -> "Text"
        else -> "Page"
      }

    fun summaryValue(value: String?): String? = value?.trim()?.takeIf(String::isNotEmpty)

    val contextSummaryLines =
      buildList {
        add("Type: ${contextLabel(event.elementInfo.contextType)}")
        summaryValue(event.elementInfo.linkUrl)?.let { add("Link URL: $it") }
        summaryValue(event.elementInfo.imageUrl)?.let { add("Image URL: $it") }
      }

    val servoItems = toDialogItems(event.items.map(::toReactNativeServoContextMenuItem), ContextMenuItemSource.SERVO)
    val appItems = toDialogItems(injectedItems, ContextMenuItemSource.APP)
    val items =
      buildList {
        contextSummaryLines.forEach { line ->
          add(
            ContextMenuDialogItem(
              title = line,
              subtitle = null,
              action = null,
              enabled = false,
              source = ContextMenuItemSource.SERVO,
              header = true
            )
          )
        }
        if (contextSummaryLines.isNotEmpty() && (servoItems.isNotEmpty() || appItems.isNotEmpty())) {
          add(
            ContextMenuDialogItem(
              title = "────────",
              subtitle = null,
              action = null,
              enabled = false,
              source = ContextMenuItemSource.SERVO,
              separator = true
            )
          )
        }
        if (servoItems.isNotEmpty()) {
          add(
            ContextMenuDialogItem(
              title = itemTitle(ContextMenuItemSource.SERVO),
              subtitle = null,
              action = null,
              enabled = false,
              source = ContextMenuItemSource.SERVO,
              header = true
            )
          )
          addAll(servoItems)
        }
        if (appItems.isNotEmpty()) {
          if (servoItems.isNotEmpty()) {
            add(
              ContextMenuDialogItem(
                title = "────────",
                subtitle = null,
                action = null,
                enabled = false,
                source = ContextMenuItemSource.APP,
                separator = true
              )
            )
          }
          add(
            ContextMenuDialogItem(
              title = itemTitle(ContextMenuItemSource.APP),
              subtitle = null,
              action = null,
              enabled = false,
              source = ContextMenuItemSource.APP,
              header = true
            )
          )
          addAll(appItems)
        }
      }
    if (items.none { it.action != null && it.enabled }) {
      activeContextMenu = null
      sendControllerCommand(dismissContextMenuCommandJson(event.contextMenuId))
      return
    }

    val adapter =
      object : ArrayAdapter<ContextMenuDialogItem>(
        context,
        android.R.layout.simple_list_item_2,
        items
      ) {
        override fun areAllItemsEnabled(): Boolean = false

        override fun isEnabled(position: Int): Boolean {
          val item = items[position]
          return item.enabled && item.action != null
        }

        override fun getView(position: Int, convertView: View?, parent: ViewGroup): View {
          val view = super.getView(position, convertView, parent)
          val item = items[position]
          val titleView = view.findViewById<TextView>(android.R.id.text1)
          val subtitleView = view.findViewById<TextView>(android.R.id.text2)
          titleView.text = item.title
          titleView.isEnabled = item.enabled
          subtitleView.text = item.subtitle.orEmpty()
          subtitleView.visibility = if (item.subtitle == null) View.GONE else View.VISIBLE
          subtitleView.isEnabled = item.enabled
          if (item.header) {
            titleView.setTextColor(Color.parseColor("#94A3B8"))
          } else if (item.separator) {
            titleView.setTextColor(Color.parseColor("#334155"))
          } else if (item.enabled) {
            titleView.setTextColor(Color.parseColor("#0F172A"))
          } else {
            titleView.setTextColor(Color.parseColor("#94A3B8"))
          }
          return view
        }
      }

    fun selectedItemFor(dialogItem: ContextMenuDialogItem): ReactNativeContextMenuItem? =
      dialogItem.action?.let { action ->
        ReactNativeContextMenuItem(
          kind = ContextMenuItemKind.ITEM,
          source = dialogItem.source,
          label = dialogItem.title,
          action = action,
          enabled = dialogItem.enabled
        )
      }

    var resolved = false
    fun resolve(dialogItem: ContextMenuDialogItem) {
      val selectedItem = selectedItemFor(dialogItem) ?: return
      if (resolved || activeContextMenu?.contextMenuId != event.contextMenuId) {
        return
      }

      resolved = true
      activeContextMenu = null
      if (selectedItem.source == ContextMenuItemSource.SERVO) {
        sendControllerCommand(
          resolveContextMenuCommandJson(event.contextMenuId, selectedItem.action.orEmpty())
        )
      } else {
        sendControllerCommand(dismissContextMenuCommandJson(event.contextMenuId))
      }
      dispatchContextMenuItemSelected(selectedItem, event.elementInfo)
    }

    fun dismiss() {
      if (resolved || activeContextMenu?.contextMenuId != event.contextMenuId) {
        return
      }

      resolved = true
      activeContextMenu = null
      sendControllerCommand(dismissContextMenuCommandJson(event.contextMenuId))
    }

    val dialog =
      AlertDialog.Builder(dialogHostContext())
        .setTitle("${contextLabel(event.elementInfo.contextType)} menu")
        .setAdapter(adapter) { _, which ->
          items.getOrNull(which)?.let(::resolve)
        }
        .create()
    activeContextMenuDialog = dialog
    dialog.setOnDismissListener {
      if (activeContextMenuDialog === dialog) {
        activeContextMenuDialog = null
      }
      if (!resolved) {
        dismiss()
      }
    }
    dialog.setOnCancelListener {
      dismiss()
    }
    dialog.show()
  }

  private fun controllerCommandJson(
    command: String,
    populate: JSONObject.() -> Unit = {}
  ): String =
    JSONObject()
      .apply {
        put("version", 1)
        put("command", command)
        populate()
      }
      .toString()

  private fun evaluationIdForJavaScriptEvaluationCommand(commandJson: String): String? =
    runCatching { JSONObject(commandJson) }
      .getOrNull()
      ?.takeIf { it.optString("command") == "evaluateJavaScript" }
      ?.optString("evaluationId")
      ?.takeIf { it.isNotBlank() }

  private fun isJavaScriptEvaluationDispatchReady(): Boolean = !disposed && isFrameLoopActive

  private fun javascriptEvaluationErrorTypeForControllerStatus(status: Int): String =
    when (status) {
      STATUS_NULL_POINTER,
      STATUS_INVALID_CONTROLLER_HANDLE,
      STATUS_EXPIRED_CONTROLLER_HANDLE -> "WebViewNotReady"
      STATUS_BACKEND_ERROR -> "InternalError"
      else -> "InternalError"
    }

  private fun resolveNavigationRequestCommandJson(navigationId: String, allow: Boolean): String =
    controllerCommandJson("resolveNavigationRequest") {
      put("navigationId", navigationId)
      put("allow", allow)
    }

  private fun resolveSimpleDialogCommandJson(
    dialogId: String,
    action: Int,
    promptValue: String?
  ): String =
    controllerCommandJson("resolveSimpleDialog") {
      put("dialogId", dialogId)
      put("confirmed", action == SIMPLE_DIALOG_ACTION_CONFIRM)
      put("promptValue", promptValue)
    }

  private fun resolveContextMenuCommandJson(contextMenuId: String, action: String): String =
    controllerCommandJson("resolveContextMenu") {
      put("contextMenuId", contextMenuId)
      put("action", action)
    }

  private fun dismissContextMenuCommandJson(contextMenuId: String): String =
    controllerCommandJson("dismissContextMenu") {
      put("contextMenuId", contextMenuId)
    }

  private fun dispatchContextMenuItemSelected(
    item: ReactNativeContextMenuItem,
    element: ContextMenuElementInformation
  ) {
    val reactContext = context as? ThemedReactContext ?: return
    val eventDispatcher = UIManagerHelper.getEventDispatcher(reactContext) ?: return
    val surfaceId = UIManagerHelper.getSurfaceId(reactContext)
    eventDispatcher.dispatchEvent(ExplorerContextMenuItemSelectedEvent(surfaceId, id, item, element))
  }

  private fun dismissActiveContextMenuDialog() {
    val dialog = activeContextMenuDialog ?: return
    activeContextMenuDialog = null
    dialog.setOnDismissListener(null)
    dialog.setOnCancelListener(null)
    dialog.dismiss()
  }

  private fun handleFilePickerRequested(event: ExplorerHostEvent.FilePickerRequested) {
    if (activeFilePicker?.filePickerId == event.filePickerId) {
      return
    }

    activeFilePicker = event
    launchFilePicker(event)
  }

  private fun handleFilePickerDismissed(event: ExplorerHostEvent.FilePickerDismissed) {
    if (activeFilePicker?.filePickerId != event.filePickerId) {
      return
    }

    clearActiveFilePicker()
  }

  private fun launchFilePicker(event: ExplorerHostEvent.FilePickerRequested) {
    val reactContext = context as? ThemedReactContext
    val activity = reactContext?.currentActivity
    if (activity == null) {
      dismissFilePicker(event.filePickerId)
      return
    }

    registerFilePickerActivityListener()

    val intent =
      Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
        addCategory(Intent.CATEGORY_OPENABLE)
        putExtra(Intent.EXTRA_ALLOW_MULTIPLE, event.allowSelectMultiple)
        val mimeTypes = mimeTypesForFilterPatterns(event.filterPatterns)
        when (mimeTypes.size) {
          0 -> type = "*/*"
          1 -> type = mimeTypes.single()
          else -> {
            type = "*/*"
            putExtra(Intent.EXTRA_MIME_TYPES, mimeTypes.toTypedArray())
          }
        }
      }

    try {
      activity.startActivityForResult(intent, FILE_PICKER_REQUEST_CODE)
    } catch (_: ActivityNotFoundException) {
      dismissFilePicker(event.filePickerId)
    }
  }

  private fun handleFilePickerResult(resultCode: Int, data: Intent?) {
    val pendingFilePicker = activeFilePicker ?: return
    if (resultCode != Activity.RESULT_OK) {
      dismissFilePicker(pendingFilePicker.filePickerId)
      return
    }

    val selectedUris = extractSelectedUris(data)
    if (selectedUris.isEmpty()) {
      dismissFilePicker(pendingFilePicker.filePickerId)
      return
    }

    Thread {
      val copiedPaths = copySelectedUrisToCache(selectedUris)
      post {
        if (disposed || activeFilePicker?.filePickerId != pendingFilePicker.filePickerId) {
          return@post
        }
        if (copiedPaths == null) {
          dismissFilePicker(pendingFilePicker.filePickerId)
          return@post
        }

        dispatchHostEvents(
          binding.resolveFilePicker(pendingFilePicker.filePickerId, copiedPaths.toTypedArray())
        )
        scheduleFrameCallback()
        clearActiveFilePicker()
      }
    }.start()
  }

  private fun extractSelectedUris(data: Intent?): List<Uri> {
    if (data == null) {
      return emptyList()
    }

    val uris = mutableListOf<Uri>()
    data.data?.let(uris::add)
    val clipData = data.clipData
    if (clipData != null) {
      for (index in 0 until clipData.itemCount) {
        clipData.getItemAt(index)?.uri?.let(uris::add)
      }
    }
    return uris.distinct()
  }

  private fun copySelectedUrisToCache(selectedUris: List<Uri>): List<String>? {
    val resolver = context.contentResolver
    val destinationDir =
      File(filePickerCacheRoot, "selection-${System.currentTimeMillis()}").apply {
        mkdirs()
      }
    if (!destinationDir.isDirectory) {
      return null
    }

    val copiedFiles = mutableListOf<String>()
    try {
      for ((index, uri) in selectedUris.withIndex()) {
        val destinationFile = File(destinationDir, "${index}-${displayNameForUri(resolver, uri, index)}")
        val input =
          resolver.openInputStream(uri) ?: run {
            destinationDir.deleteRecursively()
            return null
          }
        input.use {
          destinationFile.outputStream().use { output ->
            it.copyTo(output)
          }
        }
        copiedFiles += destinationFile.absolutePath
      }
    } catch (_: IOException) {
      destinationDir.deleteRecursively()
      return null
    }

    return copiedFiles
  }

  private fun displayNameForUri(resolver: ContentResolver, uri: Uri, index: Int): String {
    val displayName =
      resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
        if (cursor.moveToFirst()) {
          cursor.getString(0)
        } else {
          null
        }
      }

    val fallbackName =
      when {
        !displayName.isNullOrBlank() -> displayName
        !uri.lastPathSegment.isNullOrBlank() -> uri.lastPathSegment.orEmpty()
        else -> buildString {
          append("selected-file-")
          append(index)
          resolver.getType(uri)
            ?.let(MimeTypeMap.getSingleton()::getExtensionFromMimeType)
            ?.takeIf { it.isNotBlank() }
            ?.let { extension ->
              append('.')
              append(extension)
            }
        }
      }

    return fallbackName
      .replace(Regex("""[\\/:*?"<>|]"""), "_")
      .ifBlank { "selected-file-$index" }
  }

  private fun mimeTypesForFilterPatterns(filterPatterns: List<String>): List<String> =
    filterPatterns
      .mapNotNull { pattern ->
        val normalizedPattern = pattern.trim().lowercase(Locale.US)
        when {
          normalizedPattern.isBlank() -> null
          normalizedPattern.contains('/') -> normalizedPattern
          else ->
            MimeTypeMap
              .getSingleton()
              .getMimeTypeFromExtension(normalizedPattern.removePrefix("."))
        }
      }
      .distinct()

  private fun dismissFilePicker(filePickerId: String) {
    dispatchHostEvents(binding.dismissFilePicker(filePickerId))
    scheduleFrameCallback()
    clearActiveFilePicker()
  }

  private fun clearActiveFilePicker() {
    activeFilePicker = null
  }

  private fun handlePermissionRequested(event: ExplorerHostEvent.PermissionRequested) {
    if (activePermissionRequest == event && activePermissionDialog != null) {
      return
    }

    dismissActivePermissionDialog()
    activePermissionRequest = event
    showPermissionDialog(event)
  }

  private fun showPermissionDialog(event: ExplorerHostEvent.PermissionRequested) {
    var resolved = false

    fun resolve(allow: Boolean) {
      if (resolved || activePermissionRequest != event) {
        return
      }

      resolved = true
      activePermissionRequest = null
      dispatchHostEvents(binding.resolvePermission(allow))
      scheduleFrameCallback()
    }

    val title =
      event.permission
        .replaceFirstChar { if (it.isLowerCase()) it.titlecase(Locale.US) else it.toString() } +
        " permission"
    val dialog =
      AlertDialog.Builder(dialogHostContext())
        .setTitle(if (event.permission.isBlank()) "Permission request" else title)
        .setMessage("Allow ${event.origin.ifBlank { "this page" }} to access ${event.permission}?")
        .setPositiveButton("Allow") { _, _ -> resolve(true) }
        .setNegativeButton("Deny") { _, _ -> resolve(false) }
        .create()
    activePermissionDialog = dialog
    dialog.setOnDismissListener {
      if (activePermissionDialog === dialog) {
        activePermissionDialog = null
      }
      if (!resolved) {
        resolve(false)
      }
    }
    dialog.setOnCancelListener {
      resolve(false)
    }
    dialog.show()
  }

  private fun denyPendingPermissionRequest() {
    val activeRequest = activePermissionRequest
    if (activeRequest == null && !binding.hasPendingPermissionRequest()) {
      return
    }

    activePermissionRequest = null
    binding.resolvePermission(false)
  }

  private fun dismissActivePermissionDialog() {
    val dialog = activePermissionDialog ?: return
    activePermissionDialog = null
    dialog.setOnDismissListener(null)
    dialog.setOnCancelListener(null)
    dialog.dismiss()
  }

  private fun registerFilePickerActivityListener() {
    if (filePickerListenerRegistered) {
      return
    }

    val reactContext = context as? ThemedReactContext ?: return
    reactContext.addActivityEventListener(filePickerActivityListener)
    filePickerListenerRegistered = true
  }

  private fun unregisterFilePickerActivityListener() {
    if (!filePickerListenerRegistered) {
      return
    }

    val reactContext = context as? ThemedReactContext ?: return
    reactContext.removeActivityEventListener(filePickerActivityListener)
    filePickerListenerRegistered = false
  }

  private fun dispatchImeComposition(state: Int, text: String) {
    dispatchHostEvents(binding.dispatchImeComposition(state, text))
    scheduleFrameCallback()
  }

  private fun commitInputMethodValue(text: String) {
    dispatchImeComposition(IME_COMPOSITION_END, text)
  }

  private fun dismissInputMethod() {
    dispatchHostEvents(binding.dismissInputMethod())
    scheduleFrameCallback()
  }

  private fun dispatchKeyboardKey(key: Int) {
    dispatchHostEvents(binding.dispatchKeyboardKey(key))
    scheduleFrameCallback()
  }

  private fun showSimpleDialog(event: ExplorerHostEvent.SimpleDialogRequested) {
    dismissDialog(event.dialogId)

    val builder = AlertDialog.Builder(dialogHostContext())
      .setTitle("JavaScript dialog")
      .setMessage(event.message)
      .setCancelable(false)

    when (event.kind) {
      "alert" -> {
        builder.setPositiveButton(android.R.string.ok) { _, _ ->
          activeDialogs.remove(event.dialogId)
          resolveSimpleDialog(event.dialogId, SIMPLE_DIALOG_ACTION_CONFIRM, null)
        }
      }
      "confirm" -> {
        builder
          .setPositiveButton(android.R.string.ok) { _, _ ->
            activeDialogs.remove(event.dialogId)
            resolveSimpleDialog(event.dialogId, SIMPLE_DIALOG_ACTION_CONFIRM, null)
          }
          .setNegativeButton(android.R.string.cancel) { _, _ ->
            activeDialogs.remove(event.dialogId)
            resolveSimpleDialog(event.dialogId, SIMPLE_DIALOG_ACTION_DISMISS, null)
          }
      }
      "prompt" -> {
        val promptField =
          EditText(context).apply {
            setText(event.defaultValue.orEmpty())
            setSelection(text.length)
          }
        builder
          .setView(promptField)
          .setPositiveButton(android.R.string.ok) { _, _ ->
            activeDialogs.remove(event.dialogId)
            resolveSimpleDialog(
              event.dialogId,
              SIMPLE_DIALOG_ACTION_CONFIRM,
              promptField.text?.toString().orEmpty()
            )
          }
          .setNegativeButton(android.R.string.cancel) { _, _ ->
            activeDialogs.remove(event.dialogId)
            resolveSimpleDialog(event.dialogId, SIMPLE_DIALOG_ACTION_DISMISS, null)
          }
      }
      else -> {
        resolveSimpleDialog(event.dialogId, SIMPLE_DIALOG_ACTION_DISMISS, null)
        return
      }
    }

    val dialog = builder.create()
    dialog.setOnDismissListener {
      activeDialogs.remove(event.dialogId)
    }
    activeDialogs[event.dialogId] = dialog
    dialog.show()
  }

  private fun resolveSimpleDialog(dialogId: String, action: Int, promptValue: String?) {
    sendControllerCommand(resolveSimpleDialogCommandJson(dialogId, action, promptValue))
  }

  private fun dismissDialog(dialogId: String) {
    activeDialogs.remove(dialogId)?.dismiss()
  }

  private fun resolveAndDismissAllDialogs() {
    val dialogIds = (activeDialogs.keys + pendingReactNativeDialogs).distinct()
    dialogIds.forEach { dialogId ->
      binding.resolveSimpleDialog(dialogId, SIMPLE_DIALOG_ACTION_DISMISS, null)
    }
    val dialogs = activeDialogs.values.toList()
    activeDialogs.clear()
    pendingReactNativeDialogs.clear()
    dialogs.forEach(AlertDialog::dismiss)
  }

  private fun dp(value: Int): Int = (value * resources.displayMetrics.density).roundToInt()

  private fun dispatchControllerReady() {
    val reactContext = context as? ThemedReactContext ?: return
    val eventDispatcher = UIManagerHelper.getEventDispatcher(reactContext) ?: return
    val surfaceId = UIManagerHelper.getSurfaceId(reactContext)
    dispatchControllerReady(eventDispatcher, surfaceId)
  }

  private fun dispatchControllerReady(
    eventDispatcher: com.facebook.react.uimanager.events.EventDispatcher,
    surfaceId: Int
  ) {
    if (controllerReadyDispatched || disposed || id == View.NO_ID) {
      return
    }

    val controllerHandle = binding.controllerHandle() ?: return
    controllerReadyDispatched = true
    eventDispatcher.dispatchEvent(ExplorerControllerReadyEvent(surfaceId, id, controllerHandle))
  }

  private fun startFrameLoop() {
    isFrameLoopActive = true
    scheduleFrameCallback()
  }

  private fun scheduleFrameCallback() {
    if (disposed || !isFrameLoopActive || frameCallbackPosted) {
      return
    }

    frameCallbackPosted = true
    Choreographer.getInstance().postFrameCallback(frameCallback)
  }

  private fun stopFrameLoop() {
    isFrameLoopActive = false
    if (!frameCallbackPosted) {
      return
    }

    Choreographer.getInstance().removeFrameCallback(frameCallback)
    frameCallbackPosted = false
  }
}
