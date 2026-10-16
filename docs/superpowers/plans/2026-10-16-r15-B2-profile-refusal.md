# r15 B2: surface a refused profile create/update (#116)

A `CreateProfile` or `UpdateProfile` the bridge refuses (an id that is
invalid or already taken, an unknown profile) was only logged; the GUI got
nothing. Owner chose option A.

## Wire (additive)

- `ClientMessage::CreateProfile` and `UpdateProfile` gain an optional
  `request_id` (omitted when `None`) and a skipped `reply: Option<ReplyTo>`,
  exactly like `AddProfileRule`. `ws_server::stamp_reply` stamps the reply.
- The answer is the existing `ServerMessage::RuleCommandResult`: `Ok`, or
  `Refused` with the profile policy's plain-text problems. No new
  `ServerMessage`, so an older GUI is unaffected.
- Old clients send neither field and get no result; a new client talking
  to an old bridge gets none either (the field is ignored). Tests: JSON
  without the field decodes, and `None` serializes without it.

## Bridge-cli

`profile_actions::handle` answers for any of the three requests that carry
a valid request id. Wording is per request: a create/update that fails for
a store reason says the profile wasn't saved, not "the rule".

## Kirigami

`ProfilesModel` gets a `profileNote` property. `createProfile`,
`renameProfile` and `updateMatchers` send a fresh request id and remember
it; the profiles feed also routes `RuleCommandResult`s, and the result for
the remembered id sets the note (the refusal's reasons, "" on ok). The page
shows it in an `InlineMessage` under the form. A result is shown only from
the live session; a missing result shows nothing, as today.
