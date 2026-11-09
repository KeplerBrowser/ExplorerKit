# Explicit site-data transfer

This reference is for native Rust apps that want to bring selected cookies or
localStorage from a system WebKit view into Servo before the destination
navigates. Your app chooses the source, the records to move, and when to
navigate. View creation leaves both stores independent.

## Capture and initialize

Use the types in `explorerkit::site_data` and the methods on `Runtime`.

1. Ask `site_data_scope(view)` how the view shares its store. A WebKit
   `HostStore` is the named store configured by the host. A Servo
   `ProcessStore` is shared by every Servo view and session in the process.
2. Call `capture_site_data(source, selection)` on the live WebKit source.
   Set `SiteDataSelection::include_cookies` to read all cookies in its
   selected store. For localStorage, pass the origins you want to read.
   `SiteDataOrigin::parse` extracts a canonical HTTP(S) origin from a URL
   without userinfo. The scheme, host, and port identify the origin.
   Repeated origins are read once and empty maps are returned.
3. Pump the runtime on its owning thread and poll `try_take()` for the
   owned `SiteDataSnapshot`. Keep only the cookies and storage keys you
   want to import. Treat the snapshot as credentials, including HttpOnly
   cookies. Capture leaves the source view and store intact. Time the capture
   around any source changes that matter to your app.
4. Create and attach a fresh Servo destination through your normal
   view-creation path. Call `initialize_site_data(destination, snapshot)`
   before submitting navigation or JavaScript to that view. Pump until the
   operation returns a successful `SiteDataWriteSummary`. Then use
   `Runtime::load_url` on that same destination.

A successful initialization sets the supplied localStorage keys and inserts
or replaces the supplied cookie records. Unspecified storage keys stay in
place. Servo's own cookie acceptance, expiration, and eviction rules still
apply. The summary counts verified records and skipped expired cookies.
Other Servo views can see writes as they land, so coordinate when those views
use shared data. The storage and cookie rules come from [HTML Web Storage](https://html.spec.whatwg.org/multipage/webstorage.html#the-localstorage-attribute)
and [RFC 6265 storage and retrieval](https://www.rfc-editor.org/rfc/rfc6265.html#section-5.3).

## Completion and failure

Each `SiteDataOperation` delivers its result once. Call `cancel` or drop the
operation to request cancellation. Destroying the source or destination settles
its pending operation as `OwnerClosed`. The host retires private helpers
before reporting completion. Your app owns deadlines.

Admission validates the entire snapshot before any write, including origins,
cookie identities and expiries, and duplicate storage keys. A deferred Servo
view returns `NotReady` until attached. Only one initialization can run per
Servo process. Rejected admission leaves the destination available for retry.

The destination rejects navigation and JavaScript while initialization is
active. An admitted failure or cancellation keeps that destination gated
until you destroy it. An error gives a kind, stage, and mutation state without
credential contents. Once a destination write is dispatched, the operation
can fail with `MayHaveChanged`. Already submitted native work may still
finish, and partial changes remain in the process store after the view closes.

## Cookie metadata

Servo verifies observable cookie fields after inserting the batch. Its getter
does not expose the host-only bit. Native request tests cover the difference
between host-only and domain cookies. WebKit capture reports that it cannot
always distinguish omitted SameSite from explicit None and that partition
metadata is unavailable. Preserved SameSite values do not guarantee identical
cross-site request behavior between the engines.

See the [platform and build matrix](capabilities.md#site-data-transfer) for
supported hosts and directions.
