# Linked portals

A home portal can show sessions from other portals while keeping their accounts
separate. Open **Settings → Linked portals**, enter the other portal's origin,
and choose read-only or command/edit access. Approve the request on that portal
using its own login. Its administrator must first enable federation and permit
the home origin and requested scope.

The session rail and session header identify the owning portal. Commands,
permission responses, metrics and session history use that connection. A remote
login failure does not log you out of home. Session IDs alone never identify a
linked session; the connection and remote account are part of its identity.

Credentials are held in the current browser tab's memory. Reloading or closing
it requires authorization again. **Revoke and unlink** revokes the remote grant
before removing the connection. If the tab was lost, open the remote portal's
**Settings → Linked portals → Portals with access to this account** to revoke
its grant. Tokens also expire and grants have a bounded lifetime.

Optional connection-list sync stores only origins, account identifiers, labels,
instance IDs and scopes on home. It does reveal these affiliations to the home
operator. Access tokens never go to the home backend. The home frontend can
access linked credentials, so only use a home portal whose operator you trust.

## First-version boundaries

- Federation operates while the page is open; there is no cross-portal push relay.
- Forwarded sites, plugin panes and scheduling hand off to the owning portal.
- Session sharing, membership changes and deletion remain native-portal actions.
- Protected images and videos use authenticated fetches and temporary browser
  blob URLs, with a 100 MB browser limit. Bearers never appear in media URLs.
- Interrupted linked sends are not automatically replayed. Check the transcript
  before retrying an uncertain send.
- Browser origin binding is not native-client attestation. Native-shell linking
  is not enabled by this browser implementation.

For deployment and protocol details, see [the federation design](design/FEDERATION_DISCUSSION.md)
and [authentication flows](AUTH_FLOWS.md).
