package com.kepler.explorerkit.reactnative

import com.kepler.explorerkit.androidhost.ExplorerHostEvent
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ExplorerViewEventsTest {
  @Test
  fun createNewWebViewRequestedEventMapsNameAndNullablePayload() {
    val payload =
      createNewWebViewRequestedEventPayload(
        ExplorerHostEvent.PopupRequested(
          requestId = "request-1",
          parentWebViewId = "parent-1",
          parentUrl = null,
          targetUrl = null,
          windowFeatures = null,
          policy = "default-deny"
        )
      )

    assertEquals("topCreateNewWebViewRequested", CREATE_NEW_WEBVIEW_REQUESTED_EVENT_NAME)
    assertEquals("parent-1", payload.parentWebViewId)
    assertNull(payload.parentUrl)
    assertNull(payload.targetUrl)
    assertNull(payload.windowFeatures)
    assertEquals("default-deny", payload.policy)
  }

  @Test
  fun createNewWebViewRequestedEventPayloadPreservesNonNullFields() {
    val payload =
      createNewWebViewRequestedEventPayload(
        ExplorerHostEvent.PopupRequested(
          requestId = "request-1",
          parentWebViewId = "parent-1",
          parentUrl = "https://parent.test/",
          targetUrl = "https://target.test/",
          windowFeatures = "noopener,width=400",
          policy = "default-deny"
        )
      )

    assertEquals(
      CreateNewWebViewRequestedEventPayload(
        parentWebViewId = "parent-1",
        parentUrl = "https://parent.test/",
        targetUrl = "https://target.test/",
        windowFeatures = "noopener,width=400",
        policy = "default-deny"
      ),
      payload
    )
  }
}
