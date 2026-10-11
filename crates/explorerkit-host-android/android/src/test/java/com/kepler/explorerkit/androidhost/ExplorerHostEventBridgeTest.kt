package com.kepler.explorerkit.androidhost

import org.junit.Assert.assertEquals
import org.junit.Test

class ExplorerHostEventBridgeTest {
  @Test
  fun decodesScalarHostEvents() {
    val cases =
      listOf(
        """{"name":"navigationRequested","payload":{"navigationId":"navigation-1","url":"https://example.com/"}}""" to
          ExplorerHostEvent.NavigationRequested("navigation-1", "https://example.com/"),
        """{"name":"popupRequested","payload":{"requestId":"request-1","parentWebViewId":"parent-1","parentUrl":"https://parent.test/","targetUrl":null,"windowFeatures":null,"policy":"default-deny"}}""" to
          ExplorerHostEvent.PopupRequested(
            requestId = "request-1",
            parentWebViewId = "parent-1",
            parentUrl = "https://parent.test/",
            targetUrl = null,
            windowFeatures = null,
            policy = "default-deny"
          ),
        """{"name":"popupCreated","payload":{"requestId":"request-1","parentWebViewId":"parent-1","childWebViewId":"child-1","parentUrl":"https://parent.test/","targetUrl":null,"windowFeatures":null,"policy":"managed-child"}}""" to
          ExplorerHostEvent.PopupCreated(
            requestId = "request-1",
            parentWebViewId = "parent-1",
            childWebViewId = "child-1",
            parentUrl = "https://parent.test/",
            targetUrl = null,
            windowFeatures = null,
            policy = "managed-child"
          ),
        """{"name":"popupCreationFailed","payload":{"requestId":"request-1","errorMessage":"renderer unavailable"}}""" to
          ExplorerHostEvent.PopupCreationFailed("request-1", "renderer unavailable"),
        """{"name":"urlChanged","payload":{"url":"https://example.com/"}}""" to
          ExplorerHostEvent.UrlChanged("https://example.com/"),
        """{"name":"pageTitleChanged","payload":{"title":"Example Domain"}}""" to
          ExplorerHostEvent.PageTitleChanged("Example Domain"),
        """{"name":"statusTextChanged","payload":{"status":""}}""" to
          ExplorerHostEvent.StatusTextChanged(""),
        """{"name":"loadStatusChanged","payload":{"status":"Complete"}}""" to
          ExplorerHostEvent.LoadStatusChanged("Complete"),
        """{"name":"historyChanged","payload":{"entries":["https://a.example/","https://b.example/"],"current":1,"canGoBack":true,"canGoForward":false}}""" to
          ExplorerHostEvent.HistoryChanged(
            entries = listOf("https://a.example/", "https://b.example/"),
            current = 1,
            canGoBack = true,
            canGoForward = false
          ),
        """{"name":"closed","payload":{}}""" to ExplorerHostEvent.Closed,
        """{"name":"crashed","payload":{"url":null,"reason":"boom","backtrace":null}}""" to
          ExplorerHostEvent.Crashed(null, "boom", null),
        """{"name":"error","payload":{"url":"https:///","code":1,"message":"invalid url"}}""" to
          ExplorerHostEvent.Error("https:///", 1, "invalid url"),
        """{"name":"error","payload":{"url":null,"code":2,"message":"no current entry"}}""" to
          ExplorerHostEvent.Error(null, 2, "no current entry"),
        """{"name":"javascriptEvaluationResult","payload":{"evaluationId":"evaluation-1","ok":true,"valueJson":"{\"type\":\"string\",\"value\":\"Servo\"}","errorType":null}}""" to
          ExplorerHostEvent.JavaScriptEvaluationResult(
            evaluationId = "evaluation-1",
            ok = true,
            valueJson = "{\"type\":\"string\",\"value\":\"Servo\"}",
            errorType = null
          ),
        """{"name":"simpleDialogDismissed","payload":{"dialogId":"dialog-1"}}""" to
          ExplorerHostEvent.SimpleDialogDismissed("dialog-1"),
        """{"name":"inputMethodDismissed","payload":{"inputMethodId":"ime-1"}}""" to
          ExplorerHostEvent.InputMethodDismissed("ime-1"),
        """{"name":"selectElementDismissed","payload":{"selectElementId":"select-1"}}""" to
          ExplorerHostEvent.SelectElementDismissed("select-1"),
        """{"name":"contextMenuDismissed","payload":{"contextMenuId":"context-menu-1"}}""" to
          ExplorerHostEvent.ContextMenuDismissed("context-menu-1"),
        """{"name":"filePickerDismissed","payload":{"filePickerId":"file-picker-1"}}""" to
          ExplorerHostEvent.FilePickerDismissed("file-picker-1"),
        """{"name":"permissionRequested","payload":{"permission":"geolocation","origin":"http://127.0.0.1:3000"}}""" to
          ExplorerHostEvent.PermissionRequested("geolocation", "http://127.0.0.1:3000"),
        """{"name":"focusChanged","payload":{"isFocused":true}}""" to
          ExplorerHostEvent.FocusChanged(true),
        """{"name":"cursorChanged","payload":{"cursor":"pointer"}}""" to
          ExplorerHostEvent.CursorChanged("pointer"),
        """{"name":"fullscreenChanged","payload":{"isFullscreen":true}}""" to
          ExplorerHostEvent.FullscreenChanged(true),
        """{"name":"surfaceAttached","payload":{"width":640,"height":480}}""" to
          ExplorerHostEvent.SurfaceAttached(640, 480),
        """{"name":"surfaceResized","payload":{"width":800,"height":600}}""" to
          ExplorerHostEvent.SurfaceResized(800, 600),
        """{"name":"surfaceDetached","payload":{}}""" to ExplorerHostEvent.SurfaceDetached
      )

    cases.forEach { (json, expected) ->
      assertEquals(expected, ExplorerHostEventBridge.decode(json))
    }
  }

  @Test
  fun decodesSelectElementRequestWithMultipleSelectedOptions() {
    val json =
      """{"name":"selectElementRequested","payload":{"selectElementId":"select-1","options":[{"type":"option","id":1,"label":"Servo","isDisabled":false},{"type":"optgroup","label":"Engines","options":[{"id":4,"label":"Layout","isDisabled":false},{"id":9,"label":"GPU","isDisabled":true}]}],"selectedOptions":[1,4],"allowSelectMultiple":true}}"""

    assertEquals(
      ExplorerHostEvent.SelectElementRequested(
        selectElementId = "select-1",
        options =
          listOf(
            SelectElementOptionOrOptgroup.Option(
              SelectElementOption(id = 1, label = "Servo", isDisabled = false)
            ),
            SelectElementOptionOrOptgroup.Optgroup(
              label = "Engines",
              options =
                listOf(
                  SelectElementOption(id = 4, label = "Layout", isDisabled = false),
                  SelectElementOption(id = 9, label = "GPU", isDisabled = true)
                )
            )
          ),
        selectedOptions = listOf(1, 4),
        allowSelectMultiple = true
      ),
      ExplorerHostEventBridge.decode(json)
    )
  }
}
