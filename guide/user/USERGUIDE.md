# User guide

Start with [profiles and workspaces](#profiles-and-workspaces),
[documents](#documents), [writing](#writing), or [tasks](#getting-things-done).
For installation and maintenance, see [installing Koru](#installing-koru),
[updates](#updates), and [where your work is kept](#where-your-work-is-kept).
The [user guide index](README.md) also links MCP client and social-account setup.

## Socials (experimental desktop MVP)

Choose the **Socials megaphone** in the left navigator, or press **Alt+8** (`⌃⌘8` on a Mac), in
a workspace. **+ → Socials** also opens it. The left pane has Scheduled,
Drafts, expandable Channels and Connect account. This prototype saves
authoring work, connects X, LinkedIn profiles and Pages, and Reddit, and publishes
scheduled text posts from an assigned device while Koru Agent is running.
X also supports threads.

1. Choose **Connect account**, select the provider and enter your app's public
   Client ID. For X, use a Native App with
   `http://localhost:48179/oauth/x/callback` registered; follow the
   [X configuration instructions](socials/X-OAUTH.md). LinkedIn requires an
   app with native PKCE enabled by LinkedIn and permission to publish. For a
   Page, also enter its numeric **Page ID**; your authenticated member account
   must be allowed to publish for that Page. The channel uses the Page ID as its
   label. See [LinkedIn setup](socials/LINKEDIN.md). Reddit requires an
   approved installed app with identity and submit access; see
   [Reddit setup](socials/REDDIT.md). Authorize in the
   browser, then return to Koru and wait for connection to finish. Tokens are
   obtained by the browser flow and saved in this device's credential vault;
   the form does not need a Client Secret or a pasted token. You can cancel
   while waiting. LinkedIn access must be renewed through the browser when it
   expires; X and Reddit use refresh grants. Connecting an account does not
   schedule existing drafts.
2. Choose **New post**, enter the original text and select destinations.
   **Customize for each network** opens previews. **Edit** makes a destination
   independent; **Use original** restores following of the post's original
   text. X previews split a long original into a thread. An edited X variant
   is a single post and must pass the local length check. Reddit needs a subreddit
   and title; flair is entered manually where required.
3. Choose **Save draft** to retain unfinished work without a schedule.
   Saving is explicit. Ctrl+S saves the open post and keeps its current
   scheduled/draft state. Wait for Saved. If saving fails, the composer keeps
   your changes so you can retry.
4. To save a schedule, select a future local date and time and choose
   **Schedule**. Dates follow the device's locale and time zone. The calendar
   lets you change week, return to Today, filter by account or drag an editable
   post to another time. Overlapping cards appear beside each other. Choose
   **List** to see scheduled posts across dates, and **Calendar** to return.
   You can also open a post and edit its date/time without dragging it.
   Unconnected destinations still need connection;
   a scheduled card is not confirmation of publication.

The first save assigns a new post to this device for publishing. Keep that
device awake, online and running Koru Agent when the post is due. The main
window can be closed while the Agent continues. A copy synced to another
device keeps its original publishing owner; there is no automatic takeover or
cloud publisher. Older posts labeled **Saved only — no publishing device**
must use **Create a copy** before they can be scheduled. The copy starts as a
draft with a new identity and no delivery history.

When delivery starts, its text and destination details are frozen, including
Reddit's subreddit, title and flair. Each confirmed X thread part is
saved before the next part is sent. **Open published post** opens a confirmed
post on its provider. A temporary safe-to-retry failure may show **Waiting to retry**.
For **Failed** or **Needs connection**, fix the cause and use **Retry failed
delivery**; this asks the original device to continue without resending its
confirmed parts. Reconnection must be to the same provider account. A frozen old
destination with no account identity requires a new copy with a connected
account selected.

**Check publication** means the provider may have received a post even though Koru did
not obtain a confirmed result. Koru does not retry that part automatically,
and the Retry action does not override uncertainty. Check the provider account
before creating another post. **Cancel remaining posts** saves a request for
the publishing device; wait for its cancellation result. Published parts
stay on the provider, and cancellation cannot retract a request already sent. If the
publishing device is offline, the request remains pending until it returns.

Expand a channel to see **Queue** and **Posts**. Queue shows its saved upcoming
posts, soonest first. Posts shows locally confirmed publications; it does not
download an account's remote history. **Feed** and **Inbox** are disabled and
will come later. **New post** on a channel selects that account, and **Open in
schedule** opens the calendar for it. Counts describe the local saved records.
An account synced from another device needs its own connection here. Connected
on this device means a local credential is present; the remote grant can still
expire or be revoked.

The original and network forms have separate scrollbars when their controls
do not fit; long text and thread previews scroll inside their text areas.
Tab/Shift+Tab moves among currently visible controls, and Enter or Space
activates a focused action. Scroll to bring additional controls into view.

To start from an existing text document, right-click its Explore row or its
document tab and choose **New post**. Socials copies the saved text and keeps
the source's identity. Formatting, media and embedded references are excluded.
Later edits to the source do not update this copy.

Closing the tab/window or switching workspace/profile asks you to resolve
unsaved composer work first. A blocked New post or close request brings that
composer back into view with **Unsaved changes**. Save it, or use **Close inside the composer** to
discard the current unsaved changes. **Delete** asks for confirmation and
removes the post from ordinary lists. Saved content remains available after
restart, but reopen Socials through its navigator icon, Alt+8 or the + menu;
its content tab does not yet restore.

These instructions describe the experimental implementation. X single posts,
threads, cancellation and calendar layout were confirmed by the user on
September 26, 2026. Native restart/offline recovery and token rotation still
need acceptance. LinkedIn and Reddit approval, live connection and publication
checks remain pending.
The Socials surface and navigator
rows do not yet expose screen-reader semantics
or mirror its layout for RTL languages.

## Koru Cloud

Settings uses searchable Appearance, Editor, Reminders, Terminal and System
pages; `Ctrl+,` opens it with the keyboard already in the search box. Page
names, new labels/hints and search terms follow the app language.
Search also accepts the existing English technical terms. On Windows, the
play button beside the reminder tone previews it without saving a preference.
Terminal theme, font and zoom apply to terminal output independently of the
app's appearance settings. Missing spelling dictionaries can be downloaded
from the Editor page; a failed download can be retried without replacing a
working dictionary with a partial one.

On **Manage › Koru Cloud**, each Cloud Workspace shows a growing document count while
syncing and **Synced** once its document pass completes. The summary counts
completed Workspaces. An edit makes that completion pending until a later pass
confirms it. Files stay on demand and do not hold document status open; expand
a row to inspect any separate file holds. Profiles sync independently within
the device's bounded session capacity.

An enabled Workspace keeps its Cloud status between connection checks; a
temporary gap between sync sessions does not mean you turned Cloud off. A
completed document pass remains Synced until invalidated or checked again.
If the app cannot read the Workspace's sync setting, it reports unavailable.

Profiles with **Cloud sync** turned on connect through Koru Cloud automatically.
You can open locally available workspaces while Cloud connects or retries; you
do not need another device online. Manage's **Koru Cloud** page shows Cloud
connection progress for these Profiles. “Syncing via Koru Cloud” describes the connection
path, not confirmation that every document is up to date. Turning Cloud sync off
returns the Profile to its existing device-to-device connection behavior.

**The Koru Cloud page.** In the **Manage** tab, **Koru Cloud** under *This
computer* in the nav (its *On* / *Off* says whether you subscribe) shows the
subscription for your signed-in Koru account, from top to bottom:

- **When Koru can't connect**, a card at the top says so, with **Try again**
  and **Continue offline**; your local Profiles stay available. A Profile's own
  page points here with *Can't reach Koru — see Koru Cloud*.
- **Account** names the account this computer is signed in to, with
  **Accounts…** — the account chooser, which also opens from *{n} Profiles
  hidden* under the nav's Profiles. Up and Down move in it, Enter chooses,
  Escape closes it, and the wheel scrolls a long list. **Add Account** starts
  sign-in.
- **When this computer's Profiles belong to another account** than the one
  you signed in with, a card under Account says *This device has Profiles from
  another account*, with **Create a new Profile with** your email, **Sign in
  with another Google Account**, and **Continue offline**.
- **Not subscribed:** *One subscription covers every profile on this computer.
  Each signed-in profile chooses whether it syncs.*, the list of what Koru Cloud
  includes, **Subscribe to Koru Cloud** (the website's pricing page) and
  **Learn more**.
- **Subscribed:** a dot before the title, *Subscribed · syncing 1 of 2
  profiles*, and **Manage subscription**, which opens the website's dashboard,
  where you can sign in and manage the plan. A scheduled cancellation remains
  manageable until it ends.
- While the subscription is being checked, could not be checked (**Try
  again**), or you are signed out (**Sign in with Google**), the page says so in
  one line.
- **A build with no Koru Cloud** — one made from source without a Cloud
  configured — says *This build of Koru has no Koru Cloud server, so sign-in
  and Cloud sync are off.* instead. The account row reads *Not set up in this
  build*, each Profile's row *Koru Cloud isn't set up in this build*, the nav
  says *Off*, and there is no **Try again**, no **Accounts…** and no sign-in:
  this is not an outage, and nothing is retried. The first-run welcome offers
  only **Continue without an account**, and a Profile's *Sign-in and sync*
  says the same. [CONTRIBUTING.md](../../CONTRIBUTING.md) explains how to give a
  build a Koru Cloud.

The subscription is checked again each time the page comes to the front, so
return to Koru after changing the plan. This page reports billing status, not
whether documents have finished syncing through Cloud.

**Turning Cloud sync on.** Under **Which profiles sync** on the Koru Cloud
page, each Profile on this computer has a row: its tile and name, then who it is
signed in as and whether it syncs. A signed-in Profile carries a **Cloud sync**
switch; switch it on and that Profile's workspaces sync through Koru Cloud:
another device of yours receives them even when this one is closed. The row
says what is happening underneath — turning on, connecting, or syncing via Koru
Cloud — in the same words as the Profile's own page, which has the same switch
under *Sign-in and sync*. The switch is only live for a Profile of the account
you are signed in to and only while the subscription is current; otherwise it
is shown dimmed with the reason. A Profile that is not signed in says *Not
signed in* and offers **Sign in…**, which opens that Profile's page at its
*Sign-in and sync*.

**Koru asks about your Workspaces the first time.** Switching Cloud sync on
for a Profile asks whether its Workspaces should move to Cloud as well. This
question, and the switch that raises it, are on the **desktop**: a phone has no
Cloud sync switch of its own, and because each device decides for itself,
answering here does not answer it for your phone.

- **Move everything** — the Profile and every Workspace under it move together,
  and the files those Workspaces contain start being sent straight away.
- **Just this Profile** — only the Profile moves. Its Workspaces keep their
  files on this device, which is a perfectly good place for them to be; you can
  move them later by answering the question again.
- **Not now** — nothing moves and nothing is saved. Escape, and closing the
  dialog, do the same thing. Turn Cloud sync off and on again and the question
  comes back.

A Workspace you have already set yourself is left exactly as you set it,
whichever answer you give, and Koru says how many it left alone. Workspaces
you make afterwards — and ones that arrive from your other devices — follow
the Profile, so you are not asked again for each one. Nothing about
this reaches your other devices: each one decides for itself, so switching this
laptop to Cloud changes nothing on your phone.

**The keyboard does not answer the question.** Pressing Enter does nothing
until you deliberately choose a button, because the first answer can start
sending gigabytes. Escape dismisses the question without answering it — your
Profile keeps the Cloud sync you just switched on, and its Workspaces stay
exactly as they were.

**Switching Cloud sync off** stops transfers on this device immediately —
nothing waits for another device to notice. Nothing already on this device is
removed, and your other devices keep their own setting.

**How far a Profile has got.** A Profile whose Cloud sync is on has **Show
sync progress** under its row on the Koru Cloud page. Open it and the row shows
one summary line — *18 of 18 workspaces on Koru Cloud · 412 of 1,208 files · 3
held* — and one row per Workspace with its documents, its files and what it is
doing now: *27 of 27 documents · 169 of 270 files · sending · via Koru Cloud*.
A held Workspace gives its reason in the same words as that Workspace's Cloud
sync tab. *Documents* counts the documents Koru Cloud holds as they are on this
device, so it appears once Koru has checked; until then the row shows files
only. One Profile's progress is open at a time — opening another closes the
first — and **Hide sync progress** closes it. It refreshes every second while
the Koru Cloud page is the tab in front, and not at all otherwise. Every
Workspace is listed; the page scrolls. A local-only Profile shows none of
this.

**Which documents a held Workspace is waiting on.** A held Workspace's row has
a small triangle beside it: click it, or Tab to it and press Enter or Space,
and the row opens onto one line per document that uses a file Koru Cloud does
not have yet — *Trip notes · 6 files · waiting for Pixel 8*. The device named
is the one that first stored those files; when Koru knows of no such device
among yours, the line says *no device online has it* rather than guessing.
A document with six missing pictures is one line, not six, and a long list
stops after twenty lines with *and N more*. Opening a row changes nothing: it
does not retry anything or ask Koru Cloud again sooner. Click the row again to
close it. A Workspace held for another reason — a metered connection, your own
pause — opens onto *No document is waiting for another device.*

**The same, on an Android phone.** Long-press a Profile on the Profiles screen
and choose **Sync status**. The screen shows the same summary line, the same
row per Workspace and the same counts as the desktop pane, in the same words; a
held reason reads as it does on that Workspace's *Files and pictures* card. Tap
a held row — or one with files parked — to open it onto the documents it is
waiting on, twenty lines at most, and tap it again to close it. Every
Workspace is listed, since the screen scrolls. It refreshes every five seconds
while it is on screen and stops as soon as you leave it or switch apps. For a
Profile without Cloud sync on this phone, the screen says *Off, or not signed in
on this device* (or, on a build with no Koru Cloud, *Not available in this
build*).

**Files.** A file — a picture, a PDF, an archive — travels the same way a
document does, and it prefers the short route: if another of your devices is
reachable on the same network it comes from there, and Koru asks Cloud only
when it cannot. A device that goes offline part way through costs nothing but
the rest of the download — what already arrived is kept. Between two desktops
a file is never sent through a relay server, so on a network where they cannot
reach each other directly it goes through Cloud instead.

The Cloud sync tab counts the document in front of you: *Images — 7 of 7
synced* for its pictures and *Files — 1 of 1 synced* for everything else it
holds. How a whole Workspace is doing is on Manage's Koru Cloud page, which counts
**all** of its files under one heading and **never says everything is up to
date while anything is still moving.**

**Click a file and the panel explains that file.** Select one in the Files
list and the Cloud sync tab shows its name, its type and size, and where its
bytes actually are — on this device, on Koru Cloud, on both, or on neither —
with the reason underneath when something is holding it. Clear the selection
and the panel goes back to explaining the document you have open.

**A picture that has not arrived** shows a placeholder saying why, not a
guess. If another of your devices has it and is offline, the placeholder names
that device. If this device is simply not using Koru Cloud for that Workspace,
it says so and tells you to turn Cloud sync on — you do not need to go and
wake another machine.

**What "Cloud sync off" means for files.** Files follow the same setting as
everything else, per Profile and per Workspace, and per device: a Workspace
this device is not syncing sends no file to anybody and asks nobody for one,
while everything local keeps working — you can still open, edit and export
your own content. That setting is the question above: until you have answered
it for a Profile, its Workspaces move no files, and the Koru Cloud page says so
rather than leaving you to guess.
For developers reviewing the Windows UI, **F12** opens a read-only floating
inspector in its own resizable window. Its pushpin toggles **Always on top**;
the window remembers its position, size and pin setting. Point and click in the
app to inspect without activating the target; pause picking to navigate normally.
F12 resumes paused picking, or closes the inspector when picking is active.
The inspector's X always closes it and leaves the app open. The inspector
has a header ruler toggle (F11) for spacing rays. Hierarchy is the left tab;
double-click a node to open its Details on the right and bring the inspected
app window forward if it is in the background. The first version
covers shared buttons, icon buttons and selected drawing/layout seams, with
incomplete coverage clearly reported.

On Android, **Devices → Enter link code → Scan QR code** prepares Google's QR
scanner before opening its camera. The first use may need an internet connection
to download the scanner component. **Cancel** stops preparation; leaving the tab
or backgrounding Koru also prevents a delayed scanner from opening. If it cannot
start, retry after checking Google Play services or paste the link code instead.
Koru itself does not request camera access for this Google-hosted scanner.

On Android phones and tablets, documents open in read mode. Tap the pencil in
the top bar, then tap a text block to type. Changes save locally after a
1.5-second pause in typing and sync with connected devices. **Done** flushes
pending text immediately, dismisses the keyboard and returns to
reading once pending saves finish. **Enter** starts a new paragraph at the cursor
and moves typing there immediately; text after the cursor moves with it.
**Add paragraph** adds a blank input at the end. Code keeps Enter within its
block. Paragraph spacing and typography stay the same in read and edit modes. Existing formatting
and references are retained, but formatting controls and table editing are not
available yet. If saving fails, the draft stays on screen: retry or copy the text
before choosing **Use saved text**, which asks before discarding the draft.

Android's Developer screen includes an account worker row with its current
stage, enrollment target, stable failure code and retry time. During the mobile
account rollout, `account_trust_not_ready` means the account service has not
been activated; local content and existing linked devices remain usable.

How to drive Koru. The full key list is [at the end](#every-key); the rest
explains the handful of behaviours that are not what you would guess.

## Global Koru Voice on Windows

Open notification area > Voice input > Open Koru Voice. Before first enable,
the window explains that Koru types into the focused application, refuses
password/protected fields, and keeps Koru Agent available after sign-in. First
Enable also enables silent Agent launch; untick Start Koru Agent when I sign in
later if preferred. It never enables visible Workspace startup.

The shown shortcut is the chord Windows actually registered. Choose it and
press Ctrl+Shift, Ctrl+Alt, Ctrl+Win, or Shift+Win with A-Z, 0-9, Space, `[`,
`]`, `\\`, or `/` to replace it. Koru uses the physical key, so Ctrl+Alt
works even when the keyboard layout treats that modifier pair as AltGr. If Windows refuses the chord, Voice
says it is already used and leaves the previous shortcut active. If every
fallback is occupied, Start in the window and Start dictation in the tray still work.
Koru Voice closes before returning focus to the selected field; an unproved,
protected, vanished, or changed target is refused with a recovery action. Close
only hides Voice—it does not disable dictation or cancel a setting being saved.

While Koru is listening, a small status pill appears above the bottom center of
the monitor containing the selected field. It says Listening and shows a coarse
meter; Finishing replaces it after capture stops. The pill never takes focus,
even when it appears, updates, or disappears. Click its square stop control to
finish and keep the words. A shortcut/tray refusal or failure appears briefly;
an attempt from the Koru Voice window reopens that window with the full action
you can take. Opening Koru Voice settings hides the pill. When no status is
shown it performs no background animation or rendering.

There is one view: the **document editor**. A document holds prose, and it holds
**task lists** — `/List - Tasks` makes one, and inside it every line is a task
that nests, reorders and ticks. A large part of this guide is written about that
list, because it is where structure lives; the rest of a document is prose and is
covered under [Documents](#documents). Calendar and kanban are not available yet.

**The outline used to be a view of its own**, and it is gone — a task list inside
a document is what replaced it, with the same keys. Where this guide says "the
outline", read "a task list".

## Mobile foundation

You can open a Profile's **Workspaces** screen while its connection and sync
continue. If no workspaces have arrived on Android, the screen explains that
workspaces from your other devices will appear as they sync. Keep a device with
that Profile online; you can go back or use content already on this device.
The message clears when workspaces arrive. Workspace creation on mobile is not
available yet.

On Android, open **Account** from Profiles or Settings to sign in with Google
or **Sign in with Apple** and choose authorized Profiles to sync. Apple opens
in your browser; after you approve, Koru comes back by itself. If the browser
instead shows *Finish signing in to Koru*, press **Open Koru** there. The account name is separate from
the Profile name. Each Profile shows whether it is stored on this device and
whether another source device is registered; a registered source may be offline.
If there are no authorized Profiles, create one on desktop and refresh the list.
Android does not create Profiles in this flow.

Returning to Android refreshes the authorized Profile list with the saved Koru
account session. A Profile created while the app was in the background should
appear without signing in to Google again. Google sign-in is needed only when
the saved account credential can no longer be restored or refreshed.

Choose **One Profile**, **Several Profiles**, or **All current Profiles**, then
press **Apply selection**. All current Profiles captures the authorized rows
when you choose that mode. Later grants do not expand its checks or an already
submitted request; choose All again to include them. Several Profiles lets you
change individual checks, and an empty selection clears unfinished connections
without removing completed trust or local content.

When a source is unavailable, open Koru on a desktop with that Profile and check
its connection, then use **Retry Profile connection**. You can cancel enrollment
without deleting local content. Access being ready does not mean every document
has arrived: **Open local Profile** appears when the local Profile is observed.
**Continue with local content** remains available during sign-in or an outage.
Signing out keeps local content and existing QR links. If the app was stopped
before a new session was saved securely, sign in again; a saved session restores
normally. After an app update, that saved session also refreshes the device's
registered app and sync protocol versions automatically, so relinking does not
require another Google sign-in. These account screens are translated into the
eight supported Android languages; live provider and two-device acceptance
remain unverified.

Android opens Profiles, then Workspaces, then Documents from its embedded,
replicated Koru Store. Tap a document row or **Open** to read the complete page.
An outlined cloud beside the pencil means this document's Cloud transport is
currently active. If Koru cannot positively establish that state, the icon is
hidden; it does not use peer activity or account sign-in as a substitute.
The reader shows headings and formatted prose, quotes, tasks, nested bulleted,
numbered, lettered, Roman and checkbox lists, code, tables, dividers, images,
canvases, contacts and embedded references. Tapping a document link opens that
document; an in-page block link scrolls to the block, and a web link opens in
the system browser. Documents are deliberately read-only on Android in this
slice. Synchronized images are loaded through the Store and visibly fall back
when their bytes are unavailable.

### Files and pictures on a phone

A picture pasted on a desktop arrives on the phone on its own, with the desktop
switched off: the phone fetches it from Koru Cloud on the same pass that brings
the documents. While it is on the way the document shows a placeholder, and
when it lands the picture appears where the placeholder was — you do not have
to leave the document and come back. **A file shared into Koru travels exactly
the same way**, whatever it is: a PDF, a video, an archive. Everything below
says *file* because none of it has ever been about pictures only.

**Open a workspace and it says what its files are doing.** Above the Documents
and Files tabs is a card with one sentence and four numbers: how many files
this workspace refers to, how many are on this phone, how many Koru Cloud has
confirmed it holds, and how many are moving right now. While something is
moving it shows a progress bar and the size that has gone. It never says
everything is on Koru Cloud while anything is still in flight, and it is not
shown at all for a workspace with no files.

The sentence says which of these is happening, because they are different
things and only some of them are yours to fix:

| What it says | What it means |
| --- | --- |
| Moving files | Bytes are going in one direction or the other, right now |
| Every file is on Koru Cloud | Nothing outstanding, nothing moving, and Cloud has confirmed it holds this workspace |
| Waiting for an unmetered connection | A paid connection. It clears on Wi-Fi, or you can allow it here |
| Data saver is on | Android's own setting. The same allowance answers it |
| Waiting for a connection | No network. Nothing to do but wait |
| Paused | Transfers for this workspace were paused. It does not clear on its own |
| This workspace does not transfer file bytes | Its transport mode moves none. Not a pause, and asking for one file does not go through it |
| Not connected to Koru Cloud | No account has connected this workspace. Files stay here and on your linked devices |

When a count cannot be known — the pass stopped at one of those holds before it
looked — the number is shown as **—** rather than as zero. Zero would say your
pictures are not on this phone when they are sitting right there.

**On a paid connection, files wait and documents do not.** Text keeps syncing
on cellular; files and pictures are held until you are on a connection that
does not cost you, and both the card and the placeholder say so rather than
looking broken. Two ways past it: **Allow on cellular** on that card, or
**Settings → Files and pictures → Transfer on cellular**, which is the same
switch and is off until you turn it on; or **Download now** on a picture
itself, which fetches that one and leaves the rest waiting. Android's own data
saver holds files the same way and says that instead.

If the phone has no room for a file, it says so and stops — the file is not
half-written and nothing else on the phone is at risk. Free some space and it
will be fetched on the next pass. A transfer interrupted by Android closing the
app resumes from where it stopped rather than starting again, including after
the phone restarts.

Profiles, workspaces and documents are local durable copies and continue to be
available when the linked desktop is offline. iOS runs the same embedded store
and falls back to the mock services only when the native library is
unreachable, naming that reason in its diagnostics rather than impersonating a
healthy app. The Developer screen shows the platform/build, contract
version, active IDs, locale and active service implementation. Light and dark
appearance follow the device; see [the mobile guide](../../apps/android/README.md).

**Both phones name their build on the profiles screen** — `v0.1.0 #7 - branch`,
small and grey in the bottom corner — so answering "which build is this?" for a
bug report never means opening the Developer screen. The branch is there only
when the build was made from a git checkout.

Profile emoji and colour are shared appearance: changing either on desktop
updates the profile tile on linked Android and iOS devices after sync.

**Linking a device, and the devices you have linked.** On the desktop,
**Link a device…** (in the profile switcher, on the Welcome page, or at the
top of **Manage › Linked devices**) opens **Link another device** with two
tabs: **New device** shows a QR code and **Copy link code** for the other
device to use, and **Enter link code** takes the code the other device shows.
When the link succeeds the dialog says **Device linked**; **Done** (or Enter)
opens **Manage › Linked devices**, where the new device is listed. Escape
just closes it.

**Manage › Linked devices** is the one list of your devices. *This computer*
comes first: double-click its name to rename it in place — Enter saves,
Escape cancels, and clicking anywhere else saves too. Beside it is the **Use
relay** switch (below). Then every device linked to this one, each with a
small badge for its platform (WIN, MAC, AND, IOS) and a status line. **Unlink…**
asks before it removes a device; a device shown **via** another has no Unlink
here, and this computer can never be unlinked from its own list. Click a
device to see its history — what it last sent and when — on the same page;
**‹ its name** at the top (or Escape) goes back, **Load older** fetches
earlier changes, and clicking a document's update opens that document. The
list re-checks who is online every second, but only while the page is in
front. The nav's **Linked devices** row shows how many devices are linked.

**Every linked device says what it runs.** The linked-devices list — **Manage
› Linked devices** on the desktop, the Devices tab on Android — shows
under each name the version, channel, branch (development builds only) and
operating system that device last reported, and whether it is reached
*directly* or *through a relay* while online. The tone says how far apart two
devices are: a patch version apart is nothing, a minor version apart is drawn
in amber, a major version apart in red — the first thing to check when
something works on one device and not another. A row that leads with **Not
syncing · update …** names a device whose build no longer speaks the same
sync protocol as yours: it stays linked and listed, and nothing is lost, but
no content moves between the two until the named side updates. A device from
before this feature simply shows no build until it is updated.

### Sign in on another device

After Google sign-in, choose the exact existing Profile you want on this device.
Koru looks for a compatible online device that already has that Profile and
connects automatically. The waiting screen lets you continue offline or cancel;
your local Profiles remain visible either way. **Try again** on an enrollment
verification or save failure retries that connection directly rather than
starting Google sign-in again.

Each unfinished Profile connection keeps its own progress and retry time. If
several connections are pending, a slow source for one does not stop another
available Profile from connecting. Retry on a Profile row retries that Profile;
cancelling unfinished connections clears the selected account's remaining work.
Profiles already connected and local content remain available.

If connection requests are temporarily limited, Koru explains that it will retry
automatically. Your selected Profiles stay queued, and local content remains
available. Retry cannot shorten that server wait; there is no need to sign out
or restart. Android's empty Workspaces screen shows the same explanation while
that Profile is waiting for the limit to clear.

On desktop, the existing-Profile page in first-run onboarding and **Manage
Profiles → Create Profile** offers **One**, **Selected**, and **All current**.
Choose One and a row for a single Profile, or Selected and check the rows you
want. Choosing All current captures the Profiles currently authorized for this
Google identity; **Connect selected** submits those checked rows. If new grants
arrive before submission, the mode becomes Selected and keeps the original
checks. Choose All current again to include the new rows. Profiles granted later
are never added to a submitted request. Changing mode does not reconnect a
Profile already completed.
Only authorized hosted Profiles appear in this chooser. Local-only navigation
continues to follow the separate account visibility rules.

Each row distinguishes local content, a registered source that may be offline,
and no registered source yet. You can select a Profile without a source and
wait for another device. **Refresh availability** reloads those observations;
it does not connect anything. Each unfinished row shows its own progress,
failure and retry time. **Retry Profile connection** retries only that row.
**Open local content** opens an observed local copy independently of enrollment;
connected trust alone does not mean all content has arrived.

The checked draft stays with its account when you switch. Refreshing its grants
removes revoked choices, including from a parked draft. A pending submission
keeps its exact original set. If command delivery is uncertain, Koru keeps the
choice visible and offers **Cancel unfinished connections** before you choose
again; it does not guess that the old request failed. Cancellation preserves
completed trust and local content. **Create a new Profile** and **Continue
offline** remain separate actions. From the local creation form, **Return to
account choice** reopens the retained chooser. Tab moves between modes, rows and
actions; Space or Enter activates the focused choice. Escape requests unfinished
cancellation when available, or returns to the local form.

In the desktop **Manage** tab, **Connect** beside a Profile under *Other
Profiles* in the nav adds the Profile while keeping the tab open. Its row shows the current connection
status. Choose the Profile from the Profile switcher when you want to work in
it; its first active workspace opens. If the workspace is still arriving,
Koru opens the Profile immediately with no workspaces and explains that they
will appear when a device holding them comes online. It selects the first one
when it becomes available. Existing workspace selections are preserved as more
content arrives.

**Waiting for another Koru device** means no eligible peer answered: open Koru
on the other device. **Update Koru on your other device** means an online peer
is too old for this enrollment protocol. **Koru can't connect right now** means
the account discovery service is unavailable, not that another device is
offline; Koru retries with a bounded delay.

Koru never joins Profiles because emails match. It connects only the Profile
you selected, and the new device is shown as connected only after both devices
have safely saved Profile-only trust. Cancelling or signing out clears unfinished
connection intent and disables new approvals on that signed-out device, but does
not remove an established device relationship. Use **Unlink…** in **Manage ›
Linked devices** when you intend to revoke trust.

**Unlink removes a direct trust relationship.** It does not remove that device
from every other device's network. If the device you unlink introduced yours
and is your only trusted path to the others, those transitive devices disappear
from your list too. Other devices may keep their earlier view until they receive
the revocation through an available trusted path. Unlink does not erase content
already stored on either device.

On Android, a signed-in account can reconnect without a QR code. Open
**Devices → Linked devices** and choose **Relink** in the **Relink devices**
row. Koru submits all Profiles currently authorized for that account through
the normal Profile connection ceremony. The row immediately says **Waiting to
relink** and disables repeat taps. If no authorized device is online, it keeps
waiting beyond ten minutes and continues automatically when one returns.
Reopening the screen or restarting the app keeps pending enrollment waiting.
Bringing a peer online before choosing Relink does nothing: Unlink
remains a revocation. Relink creates fresh trust for a revoked relationship and
never restores its old trust record. It uses the saved Koru session and does not
open the QR scanner or Google sign-in. The row remains available when another
device is already linked so that one missing device can be recovered from a
larger set. Enrollment or scheduled retries already pending for that account
also keep Relink waiting. A terminal failure remains visible in the account
diagnostics. While the account and authorized Profiles load, the row stays
visible with loading text and a disabled action; signed-out users do not see it.

Clearing Android app storage creates a new device identity. If an already-linked
desktop later gossips the old identity back, the list shows the current phone as
**This device** and labels the same-named old row **Another installation**. That
row is historical peer identity, not this installation relinking to itself.
When it is shown **via** another device, unlink it on the named introducing
device; Android does not offer a local Unlink button for a transitive
relationship.

**Relay use is per device, and you can turn it off.** When two of your linked
devices cannot reach each other directly, they meet through Koru's own relay
server — run by Koru, not a third party. Every device has a **Use relay**
switch, on by default: on the desktop it is both in the menu of the Koru icon
in the notification area and in **Manage › Linked devices**; on a
phone it is the switch on the Devices screen (the *Linked devices* tab on
Android). Beneath each desktop switch Koru names the relay this device uses and
whether it is currently *connecting*, *connected* or *disconnected*. Turning it off stops relayed
connections for **that device only**, and immediately — no restart. Devices
on the same network, or with a working direct connection, keep syncing, and
everything on the device itself keeps working; only sync that would have to
cross the relay waits until the switch is back on or a direct path appears.
The choice is remembered on that device and never syncs to your other
devices.

A long desktop list scrolls with the **Manage › Linked devices** page, so
every retained installation — online, offline or older — and its history and
unlink action is reached by scrolling the page.

On Android, choose **Share to Koru** from another app's share sheet for a URL,
text, image, document or several files; on iOS, choose Koru from the share
sheet the same way. The destination picker leads with a **Recent** section —
the last three workspaces you shared into, so one tap saves there again — with
the full profile-and-workspace picker below it. Each item's name is editable
before saving, and each can carry an optional note that lands as the saved
file's note (on iOS the name and note fields are on the destination sheet the
app shows when a share still needs one). Each item is saved into the chosen
workspace and appears under **Files** on the Documents screen, beside anything
filed there from the desktop. Saved items are Store content, so they reach a
linked desktop the way everything else does. If one item of a multi-item share
cannot be read, the whole share is undone rather than partly saved.

**A file can carry one note** — an ordinary Koru document attached to it. A
file row whose file has a note shows a note icon to the right of the name; tap
it and the note opens like any other document, and back returns to the list. A
file without one gets **Add note** in its row's menu — long-press the row on
Android, the context menu on iOS — which creates the note and opens it. A note
never appears as a row of its own, and trashing the file trashes the note with
it.

## Global search

Press `Ctrl+P` in Koru, click the magnifying glass in the caption, or press
`Ctrl+Shift+K` anywhere on the machine — Windows and macOS both. On a Mac the
chord really is Control, not Command: a global shortcut has to dodge the
*system's* chords, and ⌘⇧K is far likelier to be taken than ⌃⇧K, so this is
the one place Koru's Ctrl does not become Cmd. The global shortcut opens a dedicated
search window. Drag the small rail above the query to reposition it; Koru
remembers that location on this computer. Drag its top or bottom edge to
increase its height; Koru remembers that height too. The in-app entries remain a fixed
620-by-400 modal over your current window. Clicking outside either search closes
it. If Koru is waiting in the notification area, the shortcut opens only search;
workspace windows stay hidden until you choose a result. On Windows, a left
click on the tray icon brings back the workspace window you used last, and
opens search only when no window is open; the menu is on the right button. Global search always
has one selected Store profile. Click the profile control at the bottom left,
or press `Ctrl+Alt+P`, to open the same profile picker used
by Koru. It opens upward over the results and shows each Store profile's name,
with the current profile in bold. Store profiles are not folder-registry rows,
so they never show a remove ✕. Results include only active workspaces
owned by that profile: workspace and document names, document text, tasks,
headings, and labelled properties. Search remembers the selected profile for
the current process session; if it is no longer available, Search selects the
first available Store profile. A profile with no eligible workspaces remains
selectable and safely shows no results.
Choosing a Store profile in Koru also makes it the profile a plain launch opens
next time. A plain click switches the current window; `Ctrl+click` explicitly
opens the selected profile in another window and keeps this one open. Saving an
older profile window's layout does not replace that most recent selection.
If Koru Store cannot be started, or the Koru app needed for a result is missing,
Search stays open and shows a warm warning banner instead of silently doing
nothing or claiming there were no results.
Type **`#`** followed by a letter or two and Search offers the tags you have,
most-used first, with the number of blocks carrying each. Pick one with the
arrow keys and `Enter`, or click it, and the word you were typing becomes the
whole tag — `#fun/home` — with a space after it. `Escape` closes the list and
changes nothing. Case and accents do not matter (`#CAF` finds `#café`), and a
`/` narrows to a branch: `#fun/h` offers the children of `fun` rather than
anything containing those letters. The list only ever offers tags that exist.

A tag in a search is a **filter on the block**, not a word to find: `Hello #fun`
finds lines that read *Hello* **and** carry `#fun`, and `#fun` on its own finds
everything under it, `#fun/home` included. Type a second `#` for a second
filter.

**Type `/` to filter by an attribute.** A `/` at the start of a word opens a
list of everything the workspaces in scope can be filtered on — *Color*, *Due*,
*Priority*, and every attribute your workspace has defined itself — narrowed as
you keep typing (`/pri` leaves *Priority*). Pick one with the arrow keys and
`Enter`, or click it.

What happens next follows the attribute's **type**, not its name:

- **A list attribute** — *Priority*, *Color*, or a workspace's own — offers its
  values straight away: `/priority ` shows *low, medium, high, urgent*, and
  choosing one writes `/priority high` into the box.
- **A word or a number** is typed into the box itself; the panel names what it
  wants. On a number, `>` and `<` compare: `/effort >3` finds everything above
  three, `/effort 3` exactly three. A value with a space in it takes quotes:
  `/notes "two words"`.
- **A date** — *Due*, or any attribute your workspace declared as a date —
  opens **the calendar**: the same month grid, the same phrase box and the same
  keys as the due picker in a document, so `2w`, `+3d`, `friday 17:30` and
  `sept 10` all work here too. `PgUp`/`PgDn` change month, the arrows move a day
  and a week, and `Enter` or a click names the day.

  A day on its own is not a filter, though — *before* it and *after* it are
  different questions — so the panel then slides across and asks **Before /
  After / Within 24 hours**. Pick one with the arrows and `Enter`, or click it.
  *Before* excludes the day itself and *After* includes it, so the two never
  overlap; *Within 24 hours* is the day and nothing after it.

  **Say the side yourself and the question is skipped.** Type `/due:after ` and
  the calendar opens already knowing which half you mean, so choosing a day
  commits on the spot. `/due:before ` and `/due:between ` do the same.

  Whatever you pick is written back into the box as text — `/due:after
  2026-09-10` — so you can read it, edit it by hand and run it again, and it
  comes back with the query in *Recent searches*.

  A due window covers deadlines set **with and without a time**: *August 30* and
  *August 30, 5 pm* are stored differently, and one `/due` filter asks about
  both.

  You can still write the whole thing yourself — `/due:before 2026-09-10`. The
  calendar only appears while the date is *empty*, so `Escape` and then type;
  clicking back into a date you have already written does not cover it either.

`Escape` closes the list — or the calendar — and changes nothing; a click
anywhere outside does the same. While either is open the arrow keys move it and
not the results, so the thing `Enter` would have opened stays where it was. The
calendar takes every key while it is up, which is what lets you type a phrase
into it; `Backspace` on an empty phrase box hands the key back and takes the
`/due` apart again.

**`/task` finds tasks.** It is the one filter with nothing to choose: a task
is a task, so `call /task` finds every task that reads *call*, wherever it
lives. `/task_state open` narrows that to the ones still open, and
`/task_state completed` to the ones finished. They compose with everything
else, so `/task /priority high /due:before 2026-10-01` is a question you can
ask in the box.

A filter and a search compose: `quarterly /priority high #fun` finds blocks that
read *quarterly*, are marked high priority, and carry `#fun`. The filter text is
not searched for — it is lifted out of the query — and because it stays visible
in the box you can edit it and run it again. A `/` that does not begin a word,
or one whose name is not an attribute, is ordinary text: `10/28` and `/tmp` are
searched for as written.

Archived and softly deleted workspaces stay out of ordinary results. If an
active global search finds nothing, **Search archived workspaces** appears when
there are archived workspaces to search; it keeps the query and searches only
those archives. Softly deleted workspaces remain excluded.

`Ctrl+P` and the caption button begin by searching only the workspace in that
window. Its empty state and recents are scoped there too, so opening an ordinary
in-app result stays in the same window. If the workspace has no match, choose
**Search this profile** to keep the query and widen that search to every active
workspace owned by the current profile. `Ctrl+Shift+K` starts in this
profile-wide scope.

An empty search shows pinned documents as a horizontal row (title only when
that pin shows its title), then up to three recently opened items; global
search also shows recent global queries. Left and Right move among pins; Down
leaves them into recents. Workspace-scoped search never surfaces query
history from another profile or workspace.
A query joins the recent-search list only after you open one of its results;
merely typing private text does not add it to history. A document- or workspace-title
match uses two lines: the matching title is highlighted, followed by its
profile/workspace path. Matches inside a document use three lines: title,
profile/workspace path, and the highlighted matching snippet. The first line is
always the containing document's title, not a repetition of the matched
paragraph. Search does not
repeat an internal field label that is already apparent from those lines. Every
term required by a multi-word search is highlighted, including repeated
occurrences in the visible snippet. Terms may be split between one result's
workspace/document path and its matching paragraph: `Roadmap down` can find a
paragraph containing **down** in the **Roadmap** workspace, with both locations
highlighted. Search never combines separate paragraphs to manufacture a match.
Common spelling mistakes are tolerated: `hould` and `shoudl` can find
**should**, while exact matches remain above fuzzy ones. Very short terms are
left literal to avoid noisy results. The active result is an inset rounded card;
selection no longer paints a full-width band.
Heading matches appear before ordinary paragraph matches. A widened or
standalone search uses the same ranking across every workspace, without
preferring the workspace you currently have open. Long lines end in an
ellipsis. The search box has a rounded
outline concentric with the search window and no separating divider. Its caret
ends on the text baseline. It supports the same selection, word movement,
clipboard, and undo behavior as Koru's other text prompts. The search box and
keyboard footer stay fixed while results between them scroll with the wheel,
trackpad, or scrollbar. As you type, the last complete result page stays in
place until the next page is ready, so the list does not flash through an empty
state. The footer says **Searching…** during that brief handoff; those retained
rows cannot be opened until they match the current input.
`Ctrl+Backspace` removes the word before the caret, just as it does in Koru's
other text prompts.
Use `Up`/`Down` or the mouse to focus a result, and `Enter` or a single click to
open it. A workspace-scoped in-app result opens in that same
window. File results use the same MIME-aware icons as Explore; opening one
switches Explore to Files, selects it, and scrolls its row into view without
launching an external application. After widening to profile-wide scope, another workspace follows the
routing described below. Switching the footer profile control searches that profile
instead. `Ctrl+Enter` or `Ctrl+click` opens the selected result in a new window
without switching the source window. Opening a result also **shows it in the
library**: every collapsed branch above the document is unfolded and the tree
scrolls the row into view, so a page found by searching can be found again by
looking. Enter does not
clear the query; after an in-app result opens, the next `Ctrl+P` restores the
same search. Closing and reopening search during the same Koru session also
restores the current query and focused result in memory, including loading later
result pages when needed. The result is checked against the refreshed search
page before it becomes active. This state is not written to collaborative
content or added to recent-query history unless a result is opened. The separate
document preview pane and multi-result selection are
not part of this release; each row carries the context needed to decide before
opening. `Esc` closes a menu first, returns from a preview-labelled Help or
Feedback screen, clears the query, then closes
search. When current pages have no matches, an in-app search can first choose
**Search this profile**. A profile-wide search with no active-workspace matches can
choose **Search archived workspaces** when any exist. Choose **Search deleted
pages** to include page Trash in the current workspace/global scope. Deleted
results open Trash with the recoverable item selected. A result reuses the most
recently active Koru window for that profile — switching workspace in place when
needed — and brings it forward. Only `Ctrl+Enter` / `Ctrl+click` starts a new
window; if no window of that profile is open, Koru launches one.
Paragraph matches land in the middle
of the document viewport so their surrounding context remains visible. The
matched authored text flashes briefly and the caret lands immediately before
it, ready to type at the match. The flash is a local navigation cue only: it
clears on interaction and is never saved or shared with collaborators. Search
icons use Koru's shared Phosphor icon set; keyboard names in the footer are text,
so they remain readable with every UI font.

The header controls are ordered **Filter**, **Sort**, and **More**. The profile
control stays at the bottom left. It is a single-choice access scope, not an
exclusion: changing it immediately replaces the result list with searches over
only that profile's workspaces. **The profile you choose is remembered on this
machine** — by Koru and by the global overlay alike, and across quitting either
one, so it does not have to be chosen again after an update or a restart.
Workspace filters are temporary exclusions for this search session and do not
change membership, permissions, or collaborative data.
`Ctrl+F` shows or hides the filter row; hiding it resets the exclusions.
`Ctrl+Alt+P` opens the profile selector while search already has focus, and
`Ctrl+O` opens sorting. Sort choices are **Relevance**, **Last modified**, and
**Last used workspace**. Search **opens on Last modified**, so the most recently
changed pages come first and the best word match decides ties; the sort control
always names the order in use, and is highlighted once you choose something
other than the default. “Last modified” means when this installation last
observed different searchable content, not an authored or shared modification
date — and it is what puts a relative age (`2h ago`) beside each result.
“Last used workspace” is based on machine-local navigation. The selected
sort remains active when search is closed and reopened during the current Koru
session. The More menu
can show keyboard hints and opens clearly labelled, read-only **Help preview**
and **Feedback preview** screens. More remains available from either preview so
the user can move between them without first returning to results. Feedback is
not sent, stored, or measured in
this phase. Search uses Koru's current theme, font roles, and global density;
there is no separate search appearance preference.
Rest an intentional hover on an icon-only control for 400 ms to see its tooltip.
It fades and settles toward the control over 100 ms; moving away fades it over
75 ms. After one tooltip has appeared, adjacent controls explain themselves
immediately for 300 ms. Keyboard focus shows the same explanation immediately.
The main-window magnifying glass identifies itself as **Search — Ctrl+P**.

If the system has already assigned `Ctrl+Shift+K` to another app, Koru falls
through to `Ctrl+Alt+Space`, then `Ctrl+Shift+O`. **Whichever
it ended up with is written beside *Search* in the agent's right-click menu** —
that is the place to look, because a released Koru has no console to print it
to. `Ctrl+P` always works while Koru is focused.

The magnifying-glass status icon at the bottom right reports local indexing
state; it uses the accent colour while work is queued, running, or needs
attention. Click it to open diagnostics. The panel shows the current workspace's
generation, pending changes, live/deleted fragment counts, last success/error,
and successful document work with total, projection, and SQLite write time,
fragment/node/dependency counts, changed-node and partition counts, generation,
mode, and completion time. The panel also reports whether the local typo
vocabulary is ready, plus active-term, tree-node, inactive-node, and build-time
counters. It initially shows the three most recent documents; choose **Show
more** for additional history. A newly indexed document is moved
to the top, including when its publication also repaired the workspace.
**Index this workspace** pauses or resumes background indexing on this device
only. Pausing keeps the last complete results available; turning it back on
schedules a catch-up. Neither the index nor this setting is stored in the shared
profile folder.

When closing the final window, choose whether Koru should stay available in the
background or quit, and optionally remember the choice. Koru Agent owns the one
notification-area icon: its menu can open Search, restore every Koru window,
open the window for one named workspace, say which build this is, check for
updates, or fully quit the agent.

## The window

The taskbar and Alt+Tab title identify the profile: ordinary windows use
**Koru — Profile**, global search uses **Koru Search — Profile**, and a popped-out
document uses **Document — Profile**. If the profile or document is renamed in
another collaborating window, open titles update when that change arrives.

A window holds **tabs**, in the same row as the minimise, maximise and close
buttons. It opens on a welcome tab showing what you had open recently, and every
tab closes — **including the last one, which closes the window**, the way a
browser does. *Close All Tabs* is the exception: it means *start again*, so it
leaves you on a fresh welcome tab rather than going away. Closing the tab you
are looking at takes you **back to the tab you were in before it** — the trail
of what you actually visited — not to whichever tab happens to sit beside it.

Down the left is the **pane** — `Ctrl+Shift+E`, or the sidebar button in the
top-left corner, which draws solid while the pane is open and says *Show
Sidebar* / *Hide Sidebar* when you point at it. It runs the
full height of the window and pushes everything else to the right. At its head
are your avatar and the workspace you are in; below them, the documents, people
and pictures in that workspace.

### With a screen reader

On Windows, **Narrator and NVDA can find their way around Koru** — the window
and which workspace you are in, the Explore column and every document in it,
the tabs, the caption's buttons, and a menu while one is open. Nothing extra
has to be switched on, and there is no separate accessible mode: it is the same
window, described.

The route to a document with no pointer is the one you would use anyway:
`Ctrl+Shift+E` opens the column and puts the keyboard in it, the arrows walk
the tree — each row read with its title, its place in the list and how deep it
sits — and **Enter** opens the one you are on. Two documents with the same name
are told apart by the tool, not by their title. `Escape` gives the keyboard
back to the document.

**The document you opened is described too.** Move into the body and you land
in a paragraph rather than at the page's edge. Each paragraph is an edit field:
the reader reads its text, the arrows move by character, `Ctrl` and the arrows
move by word, `Down` and `Up` move by visual line and on into the next
paragraph, `Shift` and the arrows select, typing replaces exactly what is
selected, and `Ctrl+Z` and `Ctrl+Shift+Z` undo and redo. Reading continues past
the bottom of the window and the page follows. A screen reader can move the
caret and select text; **it cannot write** — every edit is a real key press, the
same as for everybody else. A date or a link chip is read out after the sentence
it sits in, with the words the chip shows; it cannot be opened from a screen
reader yet.

**You can ask whether your work is saved.** Walk the chrome and you will find a
**Save status** group, just before the page. Its first item answers for the
document in front of you — *Saved.*, *Saving…*, *Kept on this device.*,
*Read-only.*, or one of the two that say something went wrong. It is silent
until you ask. If saving actually fails, or recovers, you are told **once**, and
ordinary typing and saving say nothing at all. This asks about *this computer's*
storage; it says nothing about whether another device has your change yet.

**Setting Koru up.** The first window a new person sees is described as well —
*Welcome to Koru*, the Profile step and the Workspace step, with every field,
every choice and every error. Tab walks it, Enter presses, and the whole
account-free route can be completed without a pointer. An account is never
required.

**What is not there yet.** Dialogs other than menus announce that they are there
without describing what is in them. A window split into two panes offers the
tabs and the document of the pane you are in. A document open in another tab is
not described until you switch to it. Search results, the Koru AI column, the
Trash, Settings and the tasks page are named and not yet described. Nothing is
saying anything on macOS, iOS or Linux yet.

**And this has not been listened to.** All of the above is built and checked by
machine, but nobody has yet worked through it with Narrator or NVDA end to end.
If something reads wrongly, is silent where it should speak, or speaks where it
should be quiet, that is worth telling us — it is not a thing you are doing
wrong.

| | |
| --- | --- |
| `Ctrl+T` | a new tab — a blank page, ready to type in |
| `Ctrl+Shift+A` | open **Add…** — create a document or import content |
| `Ctrl+W` | close this one — not a pinned tab; unpin or right-click Close |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | the next tab, or the previous |
| `Ctrl+1` … `Ctrl+9` | the tab in that position |

**Open tabs stay when you switch workspace.** Every document you have open remains in the strip, whichever workspace it belongs to. **Selecting a tab only changes the page** — it does not move the active workspace or the Explore pane. To land Explore on the page under a tab, use **`Ctrl+Shift+E`** or right-click the tab → **Reveal in Explore View** (same chord): that switches to the tab's home workspace if needed, focuses the tree, and scrolls the file into view.

**In Explore:** a single click opens a document in the current tab. Ctrl+click (or Shift+click) builds a multi-selection; right-click then offers **Open in new tab** (one tab each) and **Open in new window** (the whole set in one window).

**Right-click a document tab to pin it.** Pinned tabs sit on the left, show the page icon (and the title if you ask), and stay at the front of the strip. **A pinned tab keeps that document** — clicking another page in the pane, or a link on the pin, never replaces it. Unpin from the tab menu or *Remove from this list* on the Jump List. If pins are open, Welcome is not auto-opened. **+ → Welcome** selects the Welcome tab, or brings it back when it is closed. Your recent documents are under **+ → Documents**, with **Clear recents** at the bottom of the list. Settings controls whether new pins show a title; each pin can override that. Opening a page from search prefers a window that already has that tab.

On Windows, the taskbar Jump List includes **AI Chat (Ctrl+Shift+I)**. It opens
the standalone Koru AI chat, or brings the existing chat window forward.

**`+` and `Ctrl+Shift+A` open Add…**. Documents offers **New** (`Ctrl+T`) and
your ten most recent pages. Agents and Shell each offer **Active** when a
session of that kind is running — pick one to switch to it — then the same
new-profile rows as before. Chats offers **New** and your
ten most recent conversations. Import sits below a divider. `Ctrl+T` is still
the direct route to a blank page, ready to type in —
the caret opens on the first line, which shows a *Title* placeholder until you
name it, so the first thing you type is the page's name. **Nothing is saved
until you start typing**: the page appears in the library the moment you write
the first character, wherever on it the caret is. **It appears directly below
the page you made it from**, at the same level of nesting — not at the bottom of the library,
which in a workspace of a hundred documents is nowhere near where you were
working. With no page to be next to — a welcome tab, an empty workspace — it goes
to the end. Change your mind and `Escape` closes the tab as if nothing had
happened — no empty document is left behind.

**Drag a tab sideways to reorder it.** It comes with you and leaves its place
empty behind it; once you have carried it over half of a neighbour, that
neighbour slides back into the gap. Change your mind mid-drag and the same half
a tab the other way puts it back. Let go and it drops in. Click the `+` at the
end of the strip to open **Add…**. **When there are too many tabs to fit**, the
strip scrolls sideways — use the wheel over the tabs, or drag the thin bar that
appears under them while the pointer is there. **A tab's ✕ appears when you
point at it**, so the strip is labels until you want to close something.
**Clicking a document in the pane opens it in this tab**, replacing what was
there. If that page is already open, that tab comes forward instead. A pin is
left alone: the page opens in your last unpinned tab, or a new one if you have
none. Right-click (or the row `…`) offers **Open in new tab** and **Open in new
window**. **Clicking a
document pill** follows the link in this tab the same way, unless this tab is
pinned — then the link opens in a new unpinned tab (or focuses one that already
has it).

**`Alt+←` goes back to where you were, `Alt+→` forward again** (on macOS `⌘[`
and `⌘]`, because `⌥←` is word-left in every text field on that platform) — and so do the
two thumb buttons on a mouse, if yours has them. Following a document pill,
clicking something in the explore column, switching tabs and following a pill
into another workspace are all places you can come back from, and you land on
the row and the scroll position you left, not just the document. Going back and
then somewhere new drops the way you originally
came, the way a browser does. A document you have since deleted is stepped over
rather than stopping the walk. The trail is per window and lasts as long as the
window does; it is not carried across a restart.

**Pop a document tab out into its own window.** Drag the tab up or down off the
strip — about twenty pixels — and it tears loose: a pop-out marker follows the
pointer to say releasing will open a window. Let go, and the document moves to a
window of its own (still the same document, still the same undo history — type
in either and both follow). Or right-click the tab and choose **Move to New
Window**. Only
a document tab can be popped out; a welcome tab cannot.

**Split a window into panes.** Carry a torn-loose tab to an edge of the view —
the outer quarter of any side — and a tinted preview shows the pane a drop
would make: left, right, above or below. The middle of the view does nothing,
so an ordinary drop is always there; in a corner, the nearer edge wins. Let go
and the window splits: each pane has its own tab strip and its own `+`, the
hairline between them drags to resize (panes never go below 400 × 200), and
closing a pane's last tab closes the pane, not the window. **The wheel moves the
pane under the pointer**, so it does not keep scrolling the pane you last
clicked. Typing still goes to the pane you clicked, not the one you are hovering
— a pointer resting over the other half of a split cannot take your
keystrokes. **Dragging onto
another Koru window works the same way** — its edges preview the split, its
tab strip shows a marker where the tab will land, and the drop moves the tab
there instead of opening a new window. You can also drag a page straight out
of the library into any of these places.

**Right-click a tab for a menu**: Close, Close Other Tabs, Close Tabs to the
Left, Close Tabs to the Right, Close All Tabs, and — below a divider — Move to
New Window. The closing verbs are grouped together; the one row that *keeps* the
tab sits apart from them. *Close All Tabs* means *start again* and leaves you on
a fresh welcome tab; it is dimmed when only one tab is open, because starting
again where you already are does nothing you can see. An item with nothing to do
is dimmed — Move to New Window
on a welcome tab, Close Tabs to the Left on the first one. The menu opens under
the pointer and clamps inside the window; click an item to run it, or click away
(or press `Esc`) to dismiss. Drive it from the keyboard too: `↑`/`↓` move the
highlight (skipping the dimmed item, wrapping around) and `Enter` chooses.

The side panes stay hidden until you ask:

| | |
| --- | --- |
| `Ctrl+Shift+E` | **Explore** — focus the tree on this tab's document (may switch workspace). Click the arrow beside something to expand it |
| `Ctrl+Shift+J` or the title-block sparkle | **Koru AI pane** — opens the shared chat from the right, replacing Context when it is open, with the current document shown as a removable pill above the composer |
| `Ctrl+J` | **Context** — comments, assets, tasks and threads for whatever the current tab is showing, on the right. The icon at the right of the document title does the same |
| `Alt+1` / `Alt+2` / `Alt+3` | **Documents**, **Files**, **Tasks** — the three views at the head of the Explore column, in the order their icons sit; opens the column if it is closed. Hover an icon for its name and chord. **On a Mac these are `⌃⌘1` / `⌃⌘2` / `⌃⌘3`**, because Option with a digit types a character there (`¡`, `™`, `£` on a US keyboard) |
| `Alt+7` (`⌃⌘7` on a Mac) | **Canvases** — the seventh icon: every canvas in this workspace, as one list. See [Canvases](#canvases) |
| `Alt+8` (`⌃⌘8` on a Mac) | **Socials** — the eighth icon, the megaphone: scheduled posts, drafts and channels. See [Socials](#socials-experimental-desktop-mvp) |

**Drag a list to move it.** The Lists section on the tasks rail reorders the
way the document tree does: press a row, move it, and the rows under it slide
out of the way; let go and the order is kept for every window and every
device. A press that does not move is still just a press — it opens the list,
as it always did.

**One column, three modes.** The row of icons at the top of the column switches
what it is showing: your **documents**, your **files**, or your **tasks**. It is
one piece of furniture rather than three panels, so nothing moves when you swap
between them, and each mode remembers where you were in it — what you had
collapsed, where you had scrolled. The mode you were last in is the one you come
back to next time you open Koru.

The explore column lists your **documents**, nested the way you filed them. It
deliberately does *not* list a document's paragraphs — that is the document's
text, which you read in the document, and one long one would bury everything
else.

For now it does not list images, tables or code blocks either. Each of those
earned a row once, and the column filled with structure that made the documents
harder to find rather than easier; they come back when there is somewhere useful
for them to take you. A document filed *inside* one of them still appears, at the
level of the document it belongs to.

**Clicking a document takes you to it**, in a tab. If it is already open nothing
jumps.

**The two arrows in the `LIBRARY` heading fold and unfold the whole tree** — the
first shuts everything with something inside it, the second opens all of it. Both
reach the whole library, not just the part you can see, so unfolding one document
does not reveal a branch still spread out underneath it.

**What you have collapsed stays collapsed**, including across a restart and
across switching workspaces and back. It is remembered per workspace, and
anything you collapsed and then deleted is forgotten with it.

**Drag a document to move it.** Press on a document row and hold for a moment
and the pointer becomes a grab hand; move it and the row lifts and follows
while the rows around it slide fluidly out of the way, the way a tab does. The
whole subtree comes with it — collapsed or not, everything under the document
travels — though a document that was open **folds up for the move** and opens
again once it lands, so you are dragging the row, not the whole branch.
**Resting on a collapsed document springs it open** so you can drop
into it, descending level by level as you hover each child. Let go and it drops,
and **Ctrl+Z undoes the whole drag in one step.** A press that barely moves is
still a click: it opens the document
the way it always did, and until that hold elapses the cursor stays the ordinary
arrow — the grab hand is an affordance you ask for, not one every row advertises.

Where you drop decides what happens:

- on the **top quarter** of a row → it lands *before* it, as a sibling;
- on the **bottom quarter** → *after* it (and after its children);
- on the **middle** of another document → it *nests*, becoming that document's
  last child. A shut document you nest into opens up so you can see it landed.

Drag past the top or bottom of the column and the list **auto-scrolls** toward
where you are headed; the closer to the edge, the faster.

**Ctrl+click picks more than one.** Each Ctrl+click adds a document to (or
removes it from) a selection, and the next drag carries the lot together — they
land as a contiguous run where you drop, in the order they were in. A plain
click clears the selection and acts on the one row, the way it did before.

Only documents can be dragged, and only documents can have something dropped
into them.

**A name too long for the column is cut with an ellipsis**, so a row never runs
out past its edge. There is no tooltip for the rest of it yet; widening the
window is the way to read it.

**Walk the tree with the keys.** `Ctrl+Shift+E` puts the keyboard on the row you
already have open — opening the pane first if it was shut, and **putting you
back in the tree if it was already open**, so it works from the middle of typing
without reaching for the mouse. Press it again from the tree and the pane shuts,
so it is still a toggle. `↑`/`↓` move; `→` opens a row, or steps into
one that is already open; `←` shuts a row, or — from inside a branch — folds the
whole branch and lands on the row that contains it, so pressing it repeatedly
walks you out to the top. **Nothing opens until you press `Enter`**, which opens
the row you are on and gives the keyboard back to the document. `Escape` gives
it back without opening anything, and so does clicking anywhere outside the
pane. Typing always reaches the document, even while the arrows belong to the
tree.

**`F2` renames.** Point at a row in the tree and press it to rename that
document — the tab comes forward if it was not already showing. Away from the
tree, `F2` renames whatever document you are looking at. Inside a document you
can also just type over the first line, which *is* the name.

**Drag the pane's right edge to resize it**, between 180 and 500 pixels wide.
The cursor becomes a double arrow when you are over the edge, and the width in
pixels follows the edge while you drag so you can hit a number you want. The width is
remembered **per workspace**, so a workspace of deeply nested documents can have
a wide pane while another keeps a narrow one, and both come back that way. On a
window too narrow to honour it, the pane gives way rather than squeezing the
document — and the width you chose is still there when the window is wide again.

The content settles when you let go rather than following the drag. That is
deliberate: relaying a long document costs real time, and doing it for every
pixel of a drag would make the whole window stutter.

**Ask Koru AI from its own pane.** Press `Ctrl+Shift+J` or the sparkle immediately
left of the Context control on a document's title block. The title controls stay
anchored to row 0 rather than floating beside later paragraphs. The pane opens
from the right, replaces Context when it is open, carries a left-side divider,
and can be dragged from 400 to 900 logical pixels wide.
Its open state and chosen width survive restart. First use shows three cards:
**Koru AI**, OpenAI, and **On this device — Ollama**. Click a card to select it.
**Koru AI** needs no key: **Save & start chat** (or **Save & check** from
Settings) validates the hosted model. For OpenAI, paste a project API key and
choose **Save & check**. Koru saves it in the OS credential vault and immediately asks
OpenAI for every model available to that project; the model field opens the
scrollable returned list with GPT models first. A saved key has explicit
**Replace** and **Clear** actions; its value is never read back into the field.
Ollama lists locally installed models in the same field. Typing an uninstalled
name exposes an inline **Pull** action (or press `Enter`), and an empty local
catalog links to Ollama's model browser. Pull changes to **Cancel** immediately,
before Ollama has emitted its first progress update; a stopped or failed pull
can be retried. Failed reachability and an empty catalog offer **Retry**, so a
daemon or model installed while Settings is open can be discovered without
restarting Koru. If a previously ready daemon or selected model disappears,
the next check revokes readiness until validation succeeds again.
After validation, Settings offers only the
reasoning choices that the selected model reports; models without a reasoning
control show none. On a Koru AI chat, a lightbulb next to the composer turns
thinking on or off for the next send. While the model is thinking, a
**Thinking…** drawer can be expanded; after the answer it collapses to
**Reasoning**. Reloading the chat keeps the answer and drops the thoughts.

For supported OpenAI models, a globe button appears immediately left of the
model control. Turn it on to let the next replies in this chat use OpenAI web
search; the choice is sticky for this window until you turn it off or choose a
model that cannot search. Search calls can add provider charges beyond token
usage. While a search is active and no answer text has arrived, the response
says **Searching the web…**. Cited pages appear afterward in a wrapped
**Sources:** row; click a source title to open its HTTPS page. Sources are saved
with the response and return when the chat is reopened.

Long conversations use the same thin overlay scrollbar as documents: its
wider invisible gutter pages when clicked, the thumb drags without jumping,
hover or drag strengthens its colour, and the pointer becomes the vertical
resize cursor over the gutter. Opening an existing chat starts at its end once;
streaming and later relayouts do not keep it pinned there. Sending a question
moves that new user card to the top of the conversation exactly once. The
response then grows below it without pulling you away if you scroll elsewhere.
When a short response finishes, Koru retains the blank tail below it instead of
clamping the conversation back to the bottom, so the submitted question stays
where the send placed it. Sending the next question replaces that anchor.
Press `Alt+Up` to jump from the composer to your newest question and continue
toward older questions. In the workspace, the chord enters any visible AI pane
even if the document or Explore last held focus;
`Alt+Down` walks forward again, and one step past the newest question returns
the caret to the composer.

Each chat has its own workspace authority in the left control at the bottom of
the composer, immediately after the web-search globe when search is available.
Open it to select only the workspaces this chat may read or change; lists with
more than five workspaces add a filter field above the rows. Selecting none
means no workspace access. In an embedded AI pane, the
workspace shown by that window is always the first row and is selected when the
chat is opened there. You may remove that grant while the chat remains open;
opening it again in the embedded pane initializes it from the current workspace
again.

The approval control is independent and sits immediately left of the model
control. **Manual approval** lets reads proceed but pauses every proposed
change for an explicit decision. **Automatic** allows both reads and changes
within the selected workspaces. **Auto-approve this session** temporarily
approves changes for the current chat until this app session ends without
changing the durable Manual/Automatic policy. The composer footer keeps the
globe and workspace controls against its left edge and approval plus model
controls against its right edge. The send action is an arrow inside the
composer; it is disabled while the composer is empty and becomes a stop control
during an active response.

The incognito button immediately to the right of the chat title controls
**Private chat**. Turn it on to keep agents and connected assistants from
listing, searching, or reading that conversation.
You still see it everywhere in Koru, including ordinary search, and the chat's
own agent can still read the conversation it is answering. An incognito marker
in the history row and the active title button show that the chat is private.
New chats are public to tools until you turn this setting on; a branch starts
with its source chat's current setting and can then be changed independently.
A pending change offers the same session-only action beside **Approve** and
**Deny**. Tool use appears inside
the turn where it happened, immediately before the corresponding assistant
response, as a compact activity row. A document tool includes the current
document title resolved through Store; its stable id, not the title, is kept in
AI audit storage. Expand a row for status and result-size details. Failed rows
show the bounded public error reported by the tool when expanded.
A completed tool row includes its execution time in seconds after the tool name;
time spent waiting for manual approval is excluded.
A change waiting for approval expands automatically so its decisions are
visible without another click, and remains in that turn after completion. The
live three-dot pulse follows any tool rows for that attempt while Koru waits for
the first response text. It stops as soon as text arrives or the attempt ends;
an idle chat does not keep animating. Each completed assistant response shows a
quiet token-usage line underneath when the provider reported usage. The same
line returns after reopening the chat; unavailable usage is left blank rather
than estimated. Markdown tables in responses use the same presentation as
document tables: headers are bold, columns follow their content, delimiter
alignment is honored, and inline emphasis, code, and links are rendered inside
cells. If Koru cannot
load the available-workspace catalog, the composer access popover reports that
failure instead of presenting an empty list; restart the local stack and try
again. An empty list with no failure means this profile currently contains no
workspaces.

For debugging, start the stack with `KORU_AI_PAYLOAD_DEBUG=1`. AI Settings then
shows a system-prompt override for that window, and the server logs the complete
provider-neutral request JSON before conversion plus one terminal response JSON
record. On Windows, `run-all --ai` also mirrors both marked lines into its
terminal. Those records include authored chat/document content and provider
output, but never the provider credential; leave
the switch off outside a trusted development session.

The composer stays at the bottom. `Enter` sends and `Ctrl+Shift+Enter` (or
`Cmd+Shift+Enter` on macOS) inserts a newline. It grows to eight lines and then
scrolls inside itself, independently of the conversation. Embedded questions
include the active document by stable identity only after Koru has received a
durable Store acknowledgement, so the answer sees the saved revision rather
than an older snapshot. **Chats** shows the left column's Chats view with this
chat selected (see *Finding a chat again*) and **New chat** starts another
thread. Closing the pane stops its background
polling. The same durable chats open in the standalone `koru-ai-chat`
application. Double-click the title at the top of either surface to rename the
chat in place; the new title is saved with the conversation.

To collect questions about an answer, select response text, right-click, and
choose **Add comment**. Type an optional comment and press Enter; a quote pill
appears above the composer. Click a pill to revise its comment, or its close
target to remove it. You can stage several while continuing to read. Sending
turns the pills into visible Markdown blockquotes and comments above your typed
question, then clears the strip. The pills stay only in this chat surface until
send: switching chats or closing the surface discards them.

**Open Koru AI in its own window** from **Koru AI Chat** at the top of Koru
Agent's right-click menu, **AI Chat** in Koru's `+` menu, or the global
`Ctrl+Shift+I` shortcut. The menu row carries the sparkle icon and the same
shortcut hint. Agent
starts the sibling chat application when none exists and otherwise brings its
active window forward. It uses the same gateway, provider settings, and durable
conversations as the in-app pane. Its Koru-drawn caption
supports drag, double-click maximize/restore, the shared window menu, minimize,
maximize, and close. From that focused standalone chat, `Ctrl+Shift+I` returns
to the most recently active Koru workspace (or opens Koru when no workspace is
running), making the chord a toggle between the two surfaces.

If the standalone application is missing or cannot start, a request made from
a Koru workspace opens an in-window error that says the chat window was not
opened and leaves the workspace and conversations unchanged. A request owned
by Koru Agent — its menu, shortcut, or a chat result in Search — reopens
Search with an installation warning. Reinstall or restore the standalone
application for that Koru build, then try the action again.

Type `/usage` by itself to show private local measurements without contacting
the selected provider. The current chat appears first, followed by retained
usage grouped by day, surface, provider, model, reasoning effort, outcome, and
prompt version. A provider field it did not report says **unavailable**, not
zero; the report also counts web-search calls, but Koru does not estimate
monetary cost. Each history row can delete that
chat, while Settings can clear all history or clear usage data. Deleting chat
history removes its content and AI-search results but keeps anonymous,
text-free usage and feedback totals. Clearing usage removes those totals and
feedback without deleting conversations. All three destructive actions ask
for confirmation, with Cancel selected initially.

Assistant response text selects like ordinary text across wrapped lines and
Markdown blocks. Hold `Shift` with any arrow key to extend that selection;
right-click inside it for **Copy** with its `Ctrl+C` hint or **Add comment** to
stage the selected passage above the composer.
Completed responses also offer icon actions for **Copy** (the exact Markdown),
toggleable **Good** and **Bad**, **Branch**, **Store**, and **Link**. Branch opens a new durable chat
containing the conversation through that response. Store asks for a
profile/workspace and creates a complete Koru document with a backlink to the
source response. A failed or interrupted response stays in the thread with a
safe explanation and **Retry**; retry adds a new response below it. **Link**
copies a durable link to the response block. Opening one starts Koru AI if
needed, scrolls to the block, and briefly highlights it.

**Right-click Koru in the taskbar for your recent documents.** The last ten you
opened are listed under *Recent*, above *New document*; choosing one brings the
running Koru forward with that document open, or starts it there if it was not
running. An entry for a document you have since deleted does nothing.

Above *Search* is *Koru AI Chat*: it opens or focuses the standalone chat and
shows `Ctrl+Shift+I` when Agent owns that global shortcut. *Search* opens the same global search overlay the
shortcut does, and its title names the shortcut this agent actually holds — for
example *Search (Ctrl+Shift+K)*, or *Search (Ctrl+Alt+Space)* if the first choice
was already taken. It reads the agent's answer rather than assuming, so it can
never advertise a chord that does not work; before the answer arrives, or when
there is no chord, it is simply *Search*.

**`Ctrl` and the ✕ hides Koru behind Koru Agent** instead of quitting it. Every
workspace window goes; click the agent icon by the clock to open Search, or
right-click it for *Search*, *Workspaces*, the *Store* submenu, *Data
locations*, *About Koru*, *Check for updates* and *Quit Koru Agent*.

**The *Search* row names the shortcut this agent actually holds**, not the one
it asked for. If `Ctrl+Shift+K` was already taken and it fell through to
`Ctrl+Alt+Space`, that is what the menu shows — and if every candidate was taken,
or this agent was started with `--root` and took nothing machine-wide, the row
reads *Search (no global shortcut)* rather than promising a chord that does
nothing. It is the one place you can check without guessing, which matters most
on a machine running two channels at once. The
*Workspaces* submenu starts with *Open Workspace*; when Koru windows are already
open, a divider and those workspaces follow, and choosing one brings that
window to the front. *Data locations* groups *Show root*, *Show local data*
(machine-specific settings such as window geometry), and *Show remote data*
(preferences Koru and the Agent share). The *Store*
submenu reports whether the Store is running
(click the row to recheck), opens its location or current diagnostic report,
and writes a consistent one-file backup while the Store remains live. If the
Store executable is updated while the old Store is still running, Agent offers
to restart it; choosing **Yes** stops it gracefully, starts the new image, and
the open Koru windows reconnect. Koru itself and the Store it starts add no
second
notification-area icon. The plain ✕ still quits. Your work is saved on the
way in, so a machine shut down overnight loses nothing.

**Which build is this?** *About Koru* holds one row naming it — *Koru 0.4.0 ·
build 214* — and clicking it opens that exact version's release-notes page.
A build nobody published has no page, so its row is greyed and does nothing;
the version on it is still readable, which is the point of the row. Below it,
*Check for updates* asks straight away whatever automatic checks are set to.
Both are in the menu rather than only on the Welcome tab because an agent with
every window hidden has nowhere else to answer from.

**Quitting the agent takes the Store with it, when no window is open.** The
Store draws nothing and has no icon of its own, so an agent that quit and left
it running left something behind that you had no way to see. It now asks the
Store to stop on the way out — unless a Koru window is still open, in which
case that window needs it and it stays up until you close the window and quit
the agent again.

**After an update, Koru opens a *What's new* tab.** It says which version you
are now on, and below that is everything that changed in it — the same notes as
the release page, drawn as an ordinary Koru page rather than opened in your
browser. Close it like any other tab; it does not come back.

It is a page you can read and not one you can write. Scrolling, selecting,
copying and `Ctrl+F` all work; typing does nothing, and there is no `+` in the
margin and no formatting bubble, because the page is not yours to change. It is
never saved anywhere and it is not in your workspace — closing it throws it
away, and nothing you do in it can touch your own writing.

**If no Koru window was open when the update installed**, the notice waits. You
will get the tab the next time you open Koru, however long that is.

**Feedback in the overflow menu opens a Send feedback tab.** Pick general
feedback (chosen when the tab opens), an idea, or something that isn't working, write a few words (Enter starts a new line),
optionally paste or add screenshots — hover a thumbnail and press × to take
it off — and optionally attach recent application logs as a zip. Send goes
to the people building Koru and a toast thanks you for it. Closing the tab keeps a copy for this session;
opening Feedback again restores it. The tray's **Send feedback** opens the same
form in its own window.

**Workspaces → Open Workspace opens one even when nothing is running.** It used
to bring an existing window forward and do nothing at all when there was none —
which was exactly when you needed it. It now starts Koru, on the profile and
workspace you were last in. With no Profile yet it opens first-run
onboarding and creates nothing until you submit a name and Workspace. **Continue
offline** works without Agent or network access; **Sign in with Google** and
**Sign in with Apple** use the browser, then ask before selecting an existing
hosted Profile. Every local Profile remains visible offline. With Apple you may
choose **Hide My Email**; Koru then only ever sees Apple's relay address, and
Apple shares your name with Koru only the first time you sign in.

The first page has a dark Koru introduction pane beside the sign-in choices —
*Your private workspace for thinking, planning, and AI.*, the same promise as
the website's — and on a window narrower than 760 px that decorative pane steps out so the actions remain usable.
Under *Welcome to Koru* the page says an account is only needed for cloud sync
between devices. The email form (address, password, **Continue**) comes first,
then **Google** and **Apple** side by side, then — set apart below them —
**Continue without an account**. The Google action carries
Google's colored mark. A failure is said where it happened: under the email or
password field, with the field outlined in red, or in a red banner under the
button that was pressed — outlined too — with **Try again** when that attempt
can be repeated. **Terms of Service** opens
`https://www.koru.do/terms` and **Privacy Policy** opens
`https://www.koru.do/privacy`. There is no Help button and no language control
on this page.

There is no Accounts button on first-run onboarding. Use **Accounts…** on
Manage's **Koru Cloud** page after opening a Profile to switch held accounts. During Google sign-in,
**Cancel** or `Esc` cancels that sign-in and returns to the entry choice once
cancellation is confirmed. If the Agent refuses to start sign-in, the entry
choice shows the failure under the button you pressed and still offers offline
setup.

If the verified Google identity has no Koru account, native sign-in creates it
automatically. If the account has Profiles, select the checkboxes for those you
want to import and choose **Connect selected**. The first selected Profile opens
as soon as it is saved locally; its workspaces and documents can arrive afterward.
While waiting, Explore explains the pending synced data and still offers
**Create workspace**. In the **Manage** tab's nav, click **Connect** beside another
authorized Profile to add and open it on this device without leaving for the
onboarding chooser. Peer enrollment and content continue in the background.
Connected Profiles keep their account attribution even while
identity details are unavailable.

The first-run chooser also lets already-connected rows be
checked: when all selected Profiles are connected, **Open local content** opens
the first selected Profile. A mixed selection connects only the unfinished ones.

The owner-name and Workspace-name boxes behave like the document Find box:
click to place the caret, drag to select, and use the ordinary word-delete,
clipboard, and undo shortcuts. The first name belongs to the owner of the Koru
Identity. After Google sign-in it is prefilled from Google's verified profile
name when available. The purpose cards come before and fill a separate
Profile-name box with a distinct suggestion such as `Steven (Work)`. Choosing
💼 Work, 🎓 School, or 🏡 Personal updates that suggestion, so you normally only
need to review it. You can replace it; after the
first edit, changing the purpose does not overwrite it. A purpose card is only
a shortcut to that suggestion: **Continue** is available as soon as owner name
and Profile name are present, whether or not a card was chosen. On the
Workspace step, choose one of the
six template cards before naming it, or just type a name; no card is selected
initially, and a workspace named without choosing one starts Empty. Choosing a
card fills the name box with that template's name, and another card changes the
suggestion. Once you type, your name becomes authoritative: the cards stop
changing or replacing it, while the selected template remains visible. Deleting
the name back to empty hands the choice back to the cards, so you can pick a
template again. Enter in the Profile-name box continues once owner name
and Profile name are present; Enter in the Workspace-name box
creates the workspace once the name is present. Press
the workspace tile left of the name to choose its color and
emoji with the same picker available later from **Icon and color...**. **Create
workspace** saves that appearance with the first Workspace. The Profile tile
left of the Profile name opens the same Profile color-and-emoji picker used in
the application. **Back** beside Continue returns to Welcome;
Back beside Create workspace returns to the Profile form without clearing it.
Choosing the unset color uses the current theme's default tile surface (white
with a border in the light theme). A session saved by an older version with a
separate profiles tab opens on Welcome instead; the Manage tab itself comes
back after a restart.

In the **Manage** tab, **New profile** sits under the Profiles in the nav, and
the account you are signed in with is on the **Koru Cloud** page. A local-only
Profile's page says **No online identity attached** instead of reporting a
connection failure. **New profile** opens the same native first-run screens: connect it
to the signed-in Google identity, or choose **Continue offline** and create only
the local Profile and first Workspace. If you sign in with another Google
Account, existing local Profiles
stay visible and say **Connected to another account** when that exact Profile
is not authorized for the current Google identity. You can create a new Profile
for the signed-in email through the same native Profile and Workspace screens,
choose another Google Account, or continue offline. Signing in never rebinds an
existing Profile. If the account chooser names a Profile already visible on
this device, **Open local content** leaves creation and selects that Profile; a
retained removed Profile is restored before Koru switches to it. A new
connected Profile opens only after Koru has durably
created it, registered that exact Profile with the account, and saved the exact
account binding on this device. If Agent becomes unavailable after the local
Profile and Workspace are saved, Koru opens them offline instead of leaving the
creation page stuck on **Finishing...**; the account connection can be completed
later from that Profile's page in the Manage tab.

If Koru starts before the account service, the account page stays visually
stable and retries in the background after 1, 2, and 4 minutes, then at the
10-minute mark. **Try again** runs immediately without waiting for the next
automatic attempt. After 10 minutes Koru stops polling and asks you to try later
or contact support; local Profiles and **Continue offline** remain available.

An empty Profile offers **Remove Profile from this device**. A Profile that still owns
Workspaces instead offers **Remove Profile and All Workspaces...**. After one
explicit confirmation Koru closes that Profile's open Workspace sessions,
moves every active Workspace to Trash, and removes the Profile from this
device. This does not permanently erase the retained Profile/Workspace data,
remove the hosted account, or change another device. Its hosted recovery
listing remains, so signing in can show the Profile again; another trusted
device is required when no retained local copy is available. If one Workspace cannot
be moved, the Profile remains and trying again resumes from current Store
state.

Desktop native account and onboarding screens are English-only in this first release.
There is no language picker, Apple button, or email sign-in control yet; those
options are absent rather than controls that do nothing.

After the first Profile and Workspace are durable, Koru selects the Welcome tab
even when creation began in the Manage tab. It offers:

- **Customize…** — opens Settings: theme, fonts, caret color, page width.
- **Your first five minutes** — *Make your first document* (or **set up your
  mic**, which opens Settings at Voice input); *Bring your documents in*, one
  button per import format; *Bring your files in* with **Choose files…**;
  *Learn your way around*, which opens the handful of shortcuts right there;
  *Set up a reminder*, which opens All Reminders; and *Let us know how it
  went* — **feedback form** opens the Feedback tab and **email** writes to
  `support@koru.do`.
- **Connect your devices** — *Another computer* opens **Link another device**,
  and so does the square beside it; the real pairing code is in that window.
- **When you're ready to go further** — *Set up your AI* opens Providers and
  models. *Manage documents over MCP* shows the command an MCP client runs to
  reach this installation — the `koru-mcp` program beside Koru, with
  `serve --stdio` — and **Copy** (or clicking the command) puts the whole
  command on the clipboard, ready to paste into the client's settings. *Run an
  agent in a terminal* opens the list of agents from the **+** menu; choosing
  one starts it in a new tab.

Each row you complete gets a check, and the checks are kept **for this
computer**: every Profile on it sees the same ones. *n of m done* counts the
checklist; when everything is done the list folds to one line, and **Show**
opens it again. Tab and Shift+Tab move through the page, Enter presses.
Reopening Welcome focuses the same tab.
**How often Koru looks for an update depends on your channel**, because that is
what a channel is: nightly checks hourly, beta every six hours, stable once a
day. It is a check rather than a notification, so a release can be up to that
long before you hear about it — *Check for updates* in the tray asks straight
away.

**Hovering the icon names the build too** — *Koru Agent 0.4.0 — stable*, and an
explicit root in place of the channel where there is one. That matters now that
several channels can be installed side by side: two identical icons by the clock
are otherwise indistinguishable. An internally built Agent adds its source branch
as well, so agents from several worktrees remain distinguishable.

**Koru reopens where you left it** — the same tabs in the same order, the same
one selected, scrolled to where you were, with the same columns open and the
window the size and place you left it, maximised if it was.

## Task lists

Inside a list, every line is a task. Click one to put the caret in it; click the
empty page below them all and the caret goes to the end of the last line, ready
to carry on. It does not make a task — a stray click should not add work to your
list.

```
      ☐ Launch referral program                    Group  1/6
  ⣿   ☐ Reward payout flow                         🔗 Terms page
  │   │                                            │
  │   └ checkbox                                   └ a pill
  └ drag grip (appears on hover)
```

`Enter` splits the line where the caret is, so the tail becomes the next task.
`Backspace` at the very start joins the line back into the one above, and the
caret lands exactly at the seam. Both carry formatting, pills **and** any
sub-tasks with them — backspacing a heading into the line above never strands
what was underneath it.

**`Enter` on a line you have not written on yet steps back out a level**, the
same as `Shift+Tab`. That is how you finish a group of sub-tasks: Enter, Enter,
and you are back at the margin without reaching for another key. On the last
line at the margin there is nowhere further to go, so nothing happens — that
line is already the empty one waiting for your next task.

Deleting everything leaves one empty line rather than a blank page, so there is
always somewhere to start typing again.

When a list is longer than the window a thin thumb appears at the right edge.
Scroll the wheel or trackpad to move, drag the thumb to jump to a spot, or click
above or below it to page up or down. Documents get the same thumb when their
paragraphs run past the bottom.

## Structure

| | |
| --- | --- |
| `Tab` / `Shift+Tab` | indent / outdent |
| `Ctrl+Shift+↑` / `↓` | move the task up or down among its siblings |
| `Ctrl+D` | delete the task; the tasks nested under it outdent one level |

Deleting a task no longer deletes its children: the children outdent into its
place, keeping their own structure, so `Ctrl+D` on a bullet with two bullets
under it leaves those two bullets as siblings. Select a child as well and it is
deleted with the selection rather than outdented.

`Alt` with the arrows used to do this too. It no longer does anything: Windows
routes `Alt`+key through the system menu and some Linux window managers claim
it, so it was never the reliable half of the pair.

Indenting makes a task the last child of the row above it. That is why you
cannot skip a level: press `Tab` twice and the second press finds nothing new to
move under. There is no rule enforcing this — it simply cannot happen.

Dragging the grip in the left margin moves a task, and dropping it on a row at a
different depth re-parents it rather than only reordering.

### Selecting several tasks

**Drag across them.** Press in one task and pull the pointer into another, and
every task from one to the other comes with it. Dragging back shrinks it again,
so overshooting costs nothing, and running off the top or the bottom keeps
selecting to that end. Release, and the selection stays for whatever you do
next. Hold the pointer near the top or bottom of a long document and the body
scrolls in that direction while the selection keeps growing.

Within a single task the same drag selects *text*, by character. It changes to
whole tasks the moment it crosses into another one, because half of one task and
half of the next is not a thing the workspace can hold.

**Shift+click extends from the previous click.** A second click in the same
paragraph extends the text selection; a click in another block takes that block
and every one between. **Ctrl+click toggles individual blocks**, so blocks that
are not beside one another can be copied, moved or deleted together. Ctrl+click
one of them again to remove only that block; a plain click starts over.

`Shift+↑` and `Shift+↓` do the same from the keyboard, taking whole tasks once
the caret runs out of line to select — so in a list of one-line tasks, the first
press takes the next one. Pressing the opposite way shrinks the selection rather
than starting a new one.

`Ctrl+D` deletes them all, and so do `Delete` and `Backspace` — the same
behaviour, whichever is nearest — and so does **Delete** in the right-click
menu, beside Cut, Copy and Paste. A selected sub-task is deleted outright; a
sub-task that only came along because its parent was selected outdents instead
of being deleted. `Tab` and `Shift+Tab` indent them, and
`Ctrl+Shift+↑` / `↓` move the block as one. A sub-task travels with its parent
rather than being moved twice, so selecting a parent and its children and
pressing `Tab` does what you would draw on paper.

Anything else — typing, `Ctrl+B`, a plain arrow — acts on the focused task and
ends the selection.

Formatting several tasks at once is not there yet.

### Where a task with no page of its own is kept

A task you write down while reading a page belongs to that page. One you think
of on its own does not — you typed it into the Tasks view, and no page was
open. Those still have to be written somewhere, so Koru keeps a document for
them, and **you never see it**: it earns no row in the sidebar, the way the
workspace's own settings document earns none. The Tasks tab is where those
tasks are, and it finds them by their being tasks rather than by opening the
document that holds them, so there is nothing there for you to keep tidy.

Hidden is not the same as walled off. The tasks in it are yours in every
ordinary way: they sync between your machines, **search finds their words** like
any other line you have written, and everything on this page — completing,
dating, a list, a right-click — works on them exactly as it does on a task
written in a page.

**A workspace can end up with more than one of these documents**, and that is
fine rather than a mistake to fix. If you were on two computers with no
connection between them and wrote a task on each, both wrote themselves one;
when the two meet, both stay and every task in them is still yours. Nothing is
merged behind your back and nothing is thrown away. New tasks then go into
whichever of them both computers agree on, so the list stops splitting — and
since neither is a row you can see, this is bookkeeping you are told about here
rather than something you would ever notice.

### Cut, copy and paste

`Ctrl+C`, `Ctrl+X`, `Ctrl+V` and `Ctrl+Shift+V`, and what they take depends on
what is selected:

| With… | they act on |
| --- | --- |
| several tasks selected | those tasks, and everything under them |
| text selected in one task | that text |
| nothing selected | the whole task you are in, and everything under it |

That last row is the useful one: put the caret in a task, `Ctrl+C`, `Ctrl+V`, and
you have a second copy of it directly below — no selecting first.

**A task always brings its sub-tasks**, however deep, whether or not you
selected them and whether or not `F4` is hiding some of them. Cutting a task
takes the whole branch, and pasting puts the whole branch back.

**Pasting over a selection replaces it** — the same thing pasting over selected
text does inside a line. So cut a block, select where it belongs, paste, and it
has moved. With nothing selected the paste lands **below the task you are in**
instead.

Either way it keeps the shape it was copied with: a parent and its sub-tasks
come back as a parent and its sub-tasks. If the task you are standing in is
empty, the paste fills it rather than leaving a blank line above itself. Bold,
highlights, pills and the tick all travel.

**So do a block's attributes.** Copy or cut a block and its due date, its tags,
its color and its priority come with it — the same four that a duplicate keeps,
so a block reached either way is the same block. Each row of a multi-row copy
keeps its *own*: two tasks with different tags do not pool them. One `Ctrl+Z`
takes the whole paste back, attributes included.

**A custom attribute travels inside its own workspace.** An attribute the
workspace defined — `Effort: Large`, say — comes across on a paste in the same
way, so a block reached by copy and a block reached by duplicate are again the
same block. Paste it into a **different** workspace and that one is dropped:
the definition that says what `Effort` is lives in the workspace it came from,
and over there the value would be a word with nothing behind it. The block, its
text and its due date, tags, color and priority all still arrive.

**A tag pasted into another workspace is recreated there by name.** Tags belong
to a workspace, so the tag itself cannot travel — the name does. Paste a
`#home` block into a workspace that already has a `#home` (or a `#Home`, which
is the same tag) and it joins that one; paste it into a workspace that has never
had one and the tag is created, parents of a `#fun/home` path included. If the
tag cannot be created — the workspace is unreachable — the block still arrives,
without that tag.

Pasting into a **code block** brings the text alone: no marks, no pills and no
attributes, because a code block holds plain text. Text copied from another
application carries no attributes either — there are none to carry — and
pastes as plain text whichever paste you use, with Windows line endings read
as ordinary ones.

**It reaches other applications.** What you copy goes to the system clipboard as
plain text — one line per task, indented with tabs, each pill spelled out as the
label it draws, and a document's headings written as `## ` — so it pastes into a
chat window or an email as something readable. Going the other way, text copied
from anywhere becomes **one task per line**, and a leading `#` stays a `#`. A single line with no newline in it goes in at the caret rather than
making a task.

**Indentation comes across as nesting.** Tabs or spaces, and it does not matter
how many of either — the block sets its own scale, so text indented two spaces,
four spaces or one tab all arrive one level deep. Whatever the lines were
relative to each other is what you get.

In a **document** it works the other way round, because a document is prose and
its lines do not nest: pasted indentation stays as the spaces and tabs you
copied, exactly where they were in the text.

Inside Koru you get everything back exactly. Leaving through the system
clipboard, plain text is the universal half; documents also write HTML Format
so Word, OneNote and Google Docs keep headings, marks and embedded pictures.

A **code block** leaves whole. In the plain text it is a fenced Markdown block
— three backticks and its language on one line, every line of the block as
written, three backticks again — so the next editor reads a code block rather
than loose lines, and pasting that text back into Koru gives the same block
with the same language. A block that itself contains three backticks gets a
longer fence, so it still ends where it should. Where Koru writes HTML Format,
the block is a `<pre><code>` element naming the language. Inside Koru, a copied
code block keeps its language wherever it lands, alone or in the middle of a
selection of paragraphs.

Ordinary `Ctrl+V` also reads that rich HTML on the way in: semantic bold,
italic, underline, strikethrough and inline-code spans become Koru marks, while
headings, lists and quotes keep their block structure. Semantic HTML links
become Koru's labelled link pills rather than losing the destination.
Formatting carried only by a source application's private CSS classes is
ignored; the stable HTML tags are the clipboard contract.

When a clipboard format advertises **`text/markdown`**, ordinary `Ctrl+V`
parses it as CommonMark. Headings, lists, quotes, bold, italic, strikethrough,
inline code, fenced code blocks, links and GFM pipe tables become the corresponding Koru
structure. Reference definitions such as `[1]: https://… "Source"` remain as
visible labelled links at the end instead of being silently consumed by the
Markdown parser. Some applications expose Markdown only as `text/plain`; Koru
recognises that only when the text has strong Markdown evidence (for example a
heading, fenced block, link/reference, list or quote). Ordinary prose stays on
the plain/rich clipboard paths.

**`Ctrl+Shift+V` pastes only the visible text.** Headings become ordinary text,
bold and other marks are dropped, and pills become the labels you saw. It does
not round-trip through the Markdown spelling sent to other applications, so an
H2 named `Plan` becomes `Plan`, not `## Plan`. Pictures are intentionally left
out. It also bypasses Markdown detection, so `**text**` stays exactly those
characters. Use ordinary `Ctrl+V` when structure and images should travel too.

**Right-click inside a document** to see Cut, Copy, Paste and Paste as plain
text with their shortcuts. The same compact formatting toolbar used for a text
selection appears above the pointer, while the command menu opens below it. If
there is not enough room below, the menu flips above the pointer and the toolbar
stacks above the menu. Right-clicking inside selected text keeps the selection;
right-clicking elsewhere on a text row moves the caret there before the menu
opens. Right-click a document pill and the menu also offers **Open**, **Open in
new tab** and **Open in new window** — the same as click, `Ctrl+click` and
`Shift+click`.

On a rendered table cell, the same context menu adds **Edit Markdown**, row
insert/delete and column insert/delete commands. The top-left table button
switches between Compact and Extended width.

`Ctrl+V` also works in the pickers — a URL pasted into the `Ctrl+K` link prompt,
a name pasted into a rename. And in a document, `Ctrl+V` pastes a picture that
is on the clipboard — a screenshot, a browser "copy image", or image file(s)
copied in Explorer — as a new image block below the line you are on (see
[Documents](#documents)).

**A paste can carry both.** Copy a Word document full of paragraphs and inline
pictures and `Ctrl+V` into a document: the paragraphs come in as text, and the
pictures are added as image blocks after them (see
[Documents](#documents)).

### Groups and zooming — being redone

A task with children is still a group in the sense that matters: indent a row
under another and it is nested, and that nesting is stored. What has gone is the
chrome around it — the `Ctrl+G` shortcut that wrapped a row in a new parent, the
level-name chip, and zooming a subtree into its own view with a breadcrumb back
out.

They are coming back in a different shape. The nesting they operated on is
untouched, so nothing you have made is lost.

## Writing

### Dictating

An empty document hints at both ways to begin: start typing normally, or press
`Ctrl+Shift+D` to dictate.

Hold `Ctrl+Shift+D` and talk. What you say appears where you were typing —
underlined and dimmed while it is still being heard — and lands as ordinary
text when you let the key go. It goes wherever a typed character would go: a
document, the dedicated global-search window, a rename prompt, the find bar,
or the embedded Koru AI composer. The caret follows the trailing edge of the
streaming words rather than staying at their insertion point. Nothing is
written while you are still speaking, so **one dictation is one undo step**:
`Ctrl+Z` takes the whole sentence back, along with whatever was selected before
you started.

The dedicated global-search window (`Ctrl+Shift+K`) and the separate
`koru-ai-chat` application host the same push-to-talk session themselves. They
read the same consent and microphone/language/quality preferences as Koru and
ask Agent for the same root-qualified voice worker. A future Koru Terminal host
has the same rule: speech may insert text, but must never submit or execute it.

Dictation replaces a selection, exactly as typing does.

**A small chip says what is happening.** It appears beside what you are
dictating into and carries three things: a microphone, a word — *Starting*
while the model is loading, *Listening* once the microphone is open,
*Finishing* after you let go — and a nine-step meter that moves with your
voice, so you can tell the microphone is working before you find out from the
words. It also prints the shortcut, so somebody who reached dictation another
way learns the chord.

**Clicking the chip finishes the dictation and keeps the words.** It is the one
click that does not throw the sentence away, which is what it is there for:
press it instead of letting the key go, and what you have said is written.

A few things end a dictation without writing anything, and all of them do the
same thing — nothing is left behind:

- clicking, or pressing another key, before the words have finished arriving;
- `Esc`;
- the window losing focus, closing, or the machine going to sleep.

Half a sentence landing in something you were not looking at is worse than
losing it, so it is thrown away rather than guessed at.

**A code box takes no dictation.** Neither does a read-only document, or the
code you type in when linking a phone — that one is a credential, and it is not
something to say out loud.

Speech is recognised **on this machine**. Nothing is sent anywhere, and the
microphone is opened by a separate program that hands back words rather than
sound.

If holding a key while you talk does not suit, Settings can make it a toggle
instead: press once to start, press again to stop.

If something stops a dictation before it starts, it says so where this app says
everything else that has already happened — a message at the foot of the page,
which fades on its own. There are four it can do something about: the
microphone is not allowed, no voice model is installed, another window is
already dictating, and this installation has no voice program to run.

**Two other places tell you dictation is here**, and each of them prints the
chord rather than replacing it.

**`/dictate`** in a document starts a dictation at the caret — the same thing the
chord does, for when the chord is the part you cannot remember. The typed
`/dictate` is taken back out before the words arrive, and the row shows
`Ctrl+Shift+D` beside it. Since there is no key to let go of, finish that one by
pressing plain `Enter` or clicking the chip. Enter is consumed as the finish
gesture; it does not split the row underneath the provisional sentence.

`/voice-note` is reserved for a future durable audio attachment. It is not an
alias for dictation and is not offered until Koru can keep and play back audio.

Spoken editing commands are not guessed from ordinary words yet: saying
*period*, *new line*, or *new paragraph* currently produces transcript text.
The intended grammar keeps *dot* literal (it is too ambiguous), recognizes
language-specific punctuation commands, and applies line/paragraph commands as
structured edits appropriate to the focused surface.

And the notification area's **Voice input** entry is the only place to ask when
no window is open. Its title stays *Voice input* so the menu remains compact and
easy to scan; opening it says whether there is no model yet, a download is
running, or everything is ready, as well as:

- **Whether a dictation is happening now**, while one is.
- **Whether this works, and with what**: *Ready*, the quality of the model on
  this machine, and whether the model is currently in memory. A model in memory
  is why several hundred megabytes are in use; a worker with nothing loaded is
  why the next dictation pauses for a moment before it hears you.
- **The selected global Dictation chord** on Windows: the Agent tries
  `Ctrl+Shift+D`, then `Ctrl+Alt+D`, then `Ctrl+Shift+M`, and shows the one it
  actually reserved. Global insertion stays off until you accept the Koru Voice
  notice and its enable write is acknowledged; the existing in-Koru
  `Ctrl+Shift+D` and `/dictate` behavior is unchanged.
- **Input ▸**, which microphone is being listened to and every one this
  computer can see. Its own title carries the answer — *Input: Yeti Nano* —
  and says *(not connected)* when the one you chose is not plugged in, so you
  do not have to open it to find out. Choosing one here is the same choice as
  in Settings; it brings a window back if none is open, because the choice is
  yours and the window is what remembers it.
- **What went wrong last time, when something did** — *Koru is not allowed to
  use the microphone…* names where the switch is. Nothing is said when nothing
  has gone wrong.

Then two things to press. **Start dictation** brings a window forward and warms
the program that listens, so the first hold is instant; on a machine with no
model yet the same row offers to set voice input up.

And, at the bottom, two that look alike and are not:

- **Release model from memory** frees the model and leaves voice working. The
  next dictation loads it again, which takes a moment you will notice.
- **Stop voice worker** ends the program that listens altogether. The next
  dictation starts it again.

Both are greyed out rather than missing when there is nothing for them to do —
so whether they can be pressed is itself the answer to *is anything running, and
is the model loaded*.

On an installation where dictation has been switched off, the entry opens on one
line saying so, and there is nothing to press.

### Formatting

| | |
| --- | --- |
| `Ctrl+B` / `Ctrl+I` / `Ctrl+U` | bold, italic, underline |
| `Ctrl+Shift+S` | strikethrough |
| `Ctrl+Shift+H` | cycle the highlight color, and off again |
| `Ctrl+L` / `Ctrl+E` / `Ctrl+R` | align the line left, centre or right |
| `Ctrl+Alt+1` … `4` | make the line a heading; `Ctrl+Alt+0` for ordinary text |
| ``Ctrl-\`` | clear all of it, colors included |

Alignment and headings apply to the whole line, not the selection — half a line
cannot be centred, and half a line cannot be a heading. Pressing the same one
twice puts the line back to normal. `Ctrl-\` deliberately leaves both alone: it
clears what is *in* the line, not what the line is.

**Headings are documents only** — a task list gets its shape from indenting, not
from titles. There are two other ways to set one, both in [Documents](#documents).

**Lists are documents only too.** A list is a *block* — one container that wraps
its items, not a style on each line. Type `[] ` to start a task list (checkboxes),
or `/tasks` from the slash menu. The bubble's `Text ⌄` menu offers
all five styles (tasks, bullet, numbered, letter, roman) — it wraps the current
row into a new list when the caret is in prose, and re-styles the enclosing list
when the caret is already in one. `Tab` nests an item; `Shift+Tab` brings it
back out; `Enter` on an empty item steps out one level (`3 -> 2 -> 1 -> prose`).

**Every one of these works two ways.** With text selected it formats what you
selected. With nothing selected it *arms* the next thing you type — press
`Ctrl+Shift+H`, carry on typing, and the words come out highlighted. Pressing it
again while armed cycles to the next color, and once more turns it off. Moving
the caret cancels whatever is armed, so a change of mind costs nothing.

Two things worth knowing:

- **There are no `**` markers in your text.** Formatting is stored alongside the
  text rather than inside it, so it survives splitting and joining lines and
  never appears as clutter.
- **With nothing selected, `Ctrl+B` arms the *next* thing you type.** Type and it
  comes out bold; move the caret instead and the intent is dropped.
- **…unless the caret is at the very start of a line that has text**, where it
  formats **the whole line** instead. That is what the shortcut looks like it
  should do from there, and arming an invisible intent instead looked like
  nothing had happened. Press it again to take it off. The same is true of
  italic, underline, highlight and `Ctrl+\`.

Because formatting grows rightwards, typing straight after a bold word continues
in bold. `Ctrl+B` at that point turns it back off.

### Pills

A pill is a rich element sitting inline with the text — a document or a link.
Type a trigger to insert one:

| | |
| --- | --- |
| `/` | command menu — `/document` among the rest |
| `@` | document picker, directly |
| `Ctrl+K` | a link, with a name and a URL — seeded from the selection |

**There is no `/date` any more.** A date on a block is the block's **due
date** — `/due`, below — rather than an inline pill, because two ways to put
a date on a line is one too many. Date pills written before that still show
and are still editable by clicking them; nothing was converted.

 Triggers only arm at the start of a line or after a space, so the `/` in
 `http://` and the `@` in an email do not open a picker mid-word.

 **A URL you type or paste becomes a link on its own.** Type `https://example.com`
 and a space — or press Enter — and it turns into a link pill whose caption and
 target are both the URL, no `Ctrl+K` needed; pasting one (or several) does the
 same. Only a URL with a scheme is recognised, so `file.txt`, `v1.2` and a bare
 `example.com` stay text; a trailing period or comma is left as text rather than
 swallowed into the link. `Ctrl+Z` right after brings the raw URL back. A long
 URL is shown as its host — `edgetrade-my.sharepoint.com` — so the pill never
 runs off the edge of the line; hover it (or click to edit) to see the full URL.

**`Ctrl+K` reads what you have selected.** A link is two answers — a caption and
a target — and a selection is always one of them. Select a URL and it fills the
URL in, leaving you typing the caption; select a word and it fills the caption
in, leaving you typing the URL. Either way the field you land in is the one
still to be answered, and the pill **replaces what you selected** rather than
landing beside it. With nothing selected the prompt opens empty, as before.

While a picker is open, what you type stays visible in the line and filters the
list. `Esc` dismisses it and leaves the text exactly as typed — nothing is
changed until you pick something.

**Click a document pill to follow it in this tab.** `Ctrl+click` opens it in a
new tab in the background (or reuses a tab already on that document, without
switching to it). `Shift+click` opens it in a new window.

**Click a link pill to open it in your browser.** `Ctrl+click` opens the URL
without leaving Koru. Hovering a link shows where it goes, with copy and edit
buttons on the chip — copy puts the URL on the clipboard, edit reopens the
picker that made it. The pointer turns into a hand over the pill; over the
lines themselves it is an I-beam, so what you are about to click is never a
guess.

**Click a date pill to change it** — the only way one is made now, since
`/date` is gone.

A link's icon says what kind of link it is before you open it: a link for a
secure `https` page (a broken link for plain `http`), an envelope for
`mailto:`, a key for `sftp`/`ssh`, a phone for `tel:`, and so on.

A pill is a word of its own, so making one leaves a space after it and puts the
caret past the gap. Carry straight on typing, or press `/` again — which needs
that gap to be there, since a `/` in the middle of a word is just a slash.

**An arrow key stops on a pill rather than skipping past it.** The first press
puts a ring round it; a second press in the same direction carries on. Nothing
has to be pressed to leave — any other key lets go of it. Typing while a pill is
ringed does nothing, since a pill is a thing rather than a run of letters to
overtype; backspace still deletes it.

With a pill ringed, **`Enter` opens it**:

| | |
| --- | --- |
| `Enter` | **in this tab** — it follows the link in place, the way a browser does |
| `Ctrl+Enter` | in a background tab (reuse if already open), the same as `Ctrl+click` |
| `Shift+Enter` | in a new window |

If the document is already open in another tab, `Enter` takes you to that tab
rather than leaving you with two of it. On a pinned tab, `Enter` never
replaces the pin: it focuses the existing tab or opens a new unpinned one.

A date or link pill has only one thing to do, so `Enter` reopens the box
that made it whichever modifier you hold.

`Shift+Arrow` is unaffected — it is still selecting text, and runs straight
across a pill the way it always did. A pill is one **word**, so
`Ctrl+Backspace` beside one takes the pill and stops there rather than running
on through the words behind it, and one plain backspace deletes the whole thing.

Undo puts the caret back where it was before the delete, so `Ctrl+Backspace`
and `Ctrl+Z` are a round trip. A run of deletes is one `Ctrl+Z` and the caret
returns to where the run started; undoing a deleted *selection* puts it where
the selection began.

### The picker boxes

The `Ctrl+K` link boxes are proper text inputs, and
handle text the way a task does — everything except formatting and pills, which
a name and a URL have no use for:

| | |
| --- | --- |
| `Ctrl+←` / `Ctrl+→` | move by word |
| `Ctrl+Shift+←` / `→` | select by word |
| `Shift+Home` / `Shift+End` | select to either end |
| `Ctrl+Backspace` / `Ctrl+Delete` | delete by word |
| `Ctrl+A` | select all |
| `Ctrl+C` / `Ctrl+X` / `Ctrl+V` | copy, cut, paste |
| `Ctrl+Z` / `Ctrl+Shift+Z` | undo, redo |
| click | put the caret there |
| drag | select |
| double-click | select the word |

**Each box has its own undo**, and a burst of typing is one `Ctrl+Z` rather than
one per letter. It never reaches past the box: undoing in a prompt cannot
disturb the task behind it, and it stops at whatever the prompt opened with —
the URL a pill already had is not something `Ctrl+Z` should be able to erase.

A long link scrolls sideways inside its box rather than spilling out of it, so
you can work along the whole of it.

Pills are not decoration — a document pill *is* the reference, and a person
pill *is* the assignee. That is what will let the calendar and the board show
your work without you filing it anywhere twice. A **due date** is now an
attribute rather than a pill; see below.

## Tags, colors, due dates and priority

Any block — a paragraph, a list item, a whole document — can carry **tags**,
an **accent color**, a **due date** and a **priority**. They are attributes
of the block, not text in it: each shows as a small chip after the line (the
color as a dot), and clearing the attribute takes the chip away. Nothing
appears on a block that has none set.

**A chip is the control for its attribute.** Click one and the picker for that
attribute opens, over that block — the same panel the command below opens,
already showing what the block carries. The pointer tells you: over a chip it
becomes a hand. Clicking a chip does not move your caret or start a selection
in the line underneath, so you can change a due date without losing your place.
Chips on a document's **name** are clickable in exactly the same way.

**Taking an attribute off is the small `×` on its chip.** Hover any chip and an
`×` appears at its right end — on the colour dot it appears just to the right of
the dot. Clicking it removes that one thing from that one block, and one
`Ctrl+Z` puts it back: a tag chip's `×` takes off *that* tag and leaves the
others, and the due, priority and colour chips clear what they show. The chip
does not change width when the `×` appears, so nothing shuffles under your
pointer, and clicking the label (or the dot) still opens the picker. The
**Clear** row in each picker still works too — it is the same thing said with
the keyboard.

**One chip has no `×`: a task's due chip while it shows a dash.** There is no
date on it to take away, and the whole chip is the date picker instead — click
anywhere on it, including its right end. Clearing a date you *had* set works
the way it always did, and leaves the line a task with no date: the chip goes
back to a dash rather than disappearing. Stopping tracking a line as a task
altogether is a different thing, and it asks first rather than happening on a
chip.

Several ways in, all reaching the same choosers:

- **`/remind`** opens the reminder popover for the current object. Its quick
  presets, natural-language field, expanded date/time fields, optimistic
  confirmation and object-level management are described under Reminders
  below.

- **`/tag`** opens the tag chooser: type to filter your workspace's tags,
  Enter applies the highlighted one. A tag the block already carries is shown
  with a check, and **choosing a checked row takes that tag off again** — the
  list works the way a list of checkboxes does. When what you typed names
  no existing tag, the last row offers **Create tag "…"** — the tag is
  created once, for the whole workspace, and applied.

  Typing **`#tag`** in the line is the exception: those rows carry no checks,
  and typing `#home` a second time leaves you with one `#home` rather than
  none. So does applying to **several blocks at once**, where the checks can
  only show you the first block's tags.

  **It stays open, so you can apply several in one go.** The tag you chose gets
  its check straight away, the box empties ready for the next name, and the
  chooser closes when you press `Escape` or click anywhere outside it. (The
  other three pickers each set a single value, so they close as soon as you
  choose. So does the `#` shorthand below — it is one tag, typed in the line.)
- **`#`** mid-sentence works like the `:emoji:` shortcode: type `#` after a
  space and keep typing — `#mvp` — and the chooser opens over the word,
  already filtered. Enter applies the tag and takes the typed `#mvp` back
  out. At the start of a line `#` still makes a heading, exactly as before.
  It works in a document's **name** too, where it tags the document — the
  same thing `/tag` does there. A `#` in the middle of a word is just a word
  (`C# notes` types straight through), and a `#` followed by one character
  that names no tag opens nothing.
- **`/color`** lists nine named accents with a swatch beside each; the one
  the block wears is checked, and a **Clear** row appears while one is set.
- **`/due`** opens the month grid and sets the block's due date. It is the
  only way to put a date on a block — the old `/date`, which made an inline
  date pill instead, is gone. If a due is already set the grid
  opens on it, and a **Clear** row appears under the grid. `Delete` or
  `Backspace` clears it while the phrase box is empty, and **`Alt+C`** clears it
  at any time — which is the underline under the row's `C`, and the only way to
  reach Clear once you have started typing a phrase (the box takes `Delete` and
  `Backspace` for the caret then). A date-only due is a calendar date —
  the same day for every collaborator in every timezone.

  **Above the grid is a box you can type a phrase into** — `friday`,
  `friday 17:30`, `5pm`, `+4h`, `aug 31 at 9:15pm`. It takes
  everything the typed `/due` argument takes; the grammar is tabulated below.
  A grid can
  only name a day, so the box is how you say a *time* or a *relative* offset
  without going back to the command menu.

  **Enter moves the calendar rather than closing the picker.** Type `2w` and
  press Enter and the highlighted day jumps two weeks out — the box empties,
  the panel stays open, and nothing is set yet. Type `+3d` and Enter and it
  moves three days further; `-1w` takes it back a week. The `+` and `-` count
  from **the day you are looking at**, so the box is a small calculator for
  aiming the grid. **Press Enter again on the empty box to commit** the day you
  landed on — or click it, which commits too. (Two exceptions, both because a
  month grid has no cell for a clock: a phrase with a *time* in it — `5pm`,
  `friday 17:30`, `+4h` — commits straight away and closes, and a phrase Koru
  does not know does nothing at all: nothing set, nothing cleared, your text
  still in the box and the grid still there to click.)

  While the box is empty the arrow keys move the grid
  a day at a time as they always did; once you have typed something, ← and →
  move the caret in the box instead. ↑/↓ (a week) and `PgUp`/`PgDn` (a month)
  are always the grid's.
- **`/priority`** lists the four fixed levels — Urgent, High, Medium, Low —
  most important first, with the current one checked and a **Clear** row
  while one is set. Unset is the normal state; there is no fifth level.
- **`/task`** starts tracking the line as a task, and there is nothing left to
  choose — it applies the moment you pick it. Nothing about the line moves:
  the same block, in the same place, with the same words. What appears is the
  **due chip**, showing a dash while the task has no date; setting one later,
  through `/due` or by clicking that chip, fills it in. `/due` on a plain line
  does both halves at once, which is why there is no `/task friday` — say the
  date and the task comes with it.

  It works on **any line a task can be**: a paragraph, a checklist item, an
  image, and a document's own name (which makes the page a task — see below).
  On a quote, a code block, a table or a divider the row is still listed but
  **dim, with *Can't be a task* beside it**, because those keep no task state
  at all; picking it does nothing and leaves the explanation on screen. The
  way round it is the one worth having: put a checklist item nearby — *Review
  the quote* — and make **that** the task.
- **`/task_list`** puts the line in one of your lists. The picker shows the
  workspace's lists with their colours, the one the line is already in
  checked and a **Clear** row while it is in one; type to filter. A name no
  list has yet offers **New list "…"** — choose it and the swatches follow,
  with the first colour no list is wearing already picked; Enter makes the
  list and files the line in it. A line that is not a task becomes one on
  the way, since a list holds tasks — one Ctrl+Z takes back the membership,
  the marker and the typed command together.
- **`/completed`** finishes the task the line is — or reopens it when it is
  done — and is dimmed, *Not a task*, on a line that is not one. On a page
  whose title is the task, this is how the page is finished.

**Typed arguments** skip the picker. In the command menu's search box, a
command name (or an unambiguous prefix), a space, and a value commit
directly on Enter:

- `/due tomorrow`, `/due friday`, `/due aug 31`, `/due tomorrow 9am`,
  `/due friday 17:30`, `/due 5pm`, `/due 2w`, `/due +30m` — plain-English
  dates, times and durations, tabulated below. If what you typed does not parse, the month
  grid opens instead — Enter never silently does nothing.
- `/priority high` — or just `/pri h`; the four levels have distinct first
  letters.

### Every way to write a due date

The same grammar in both places: after `/due` in the command box, and in the
due picker's own phrase box. Case and surrounding spaces do not matter, and a
comma is ignored.

Two kinds of answer, and the difference is deliberate. A **day** is the same
day for everyone, everywhere — no clock is stored, so nobody's Tuesday becomes
somebody else's Monday. A **time** stores an exact moment, so collaborators in
different places agree on when it is. Anything you write without a clock in it
gives a day.

| What you type | You get | Meaning |
| --- | --- | --- |
| `today`, `tomorrow`, `yesterday` | day | Relative to your own clock. |
| `monday` … `sunday` | day | Always the **next** one. `friday` typed on a Friday means next Friday, never today. |
| `mon`, `tue`, `tues`, `wed`, `thu`, `thur`, `thurs`, `fri`, `sat`, `sun` | day | The same, abbreviated. |
| `aug 31`, `august 31` | day | This year's, unless that day has passed — then next year's. |
| `31 aug`, `31 august` | day | Same, the other way round. |
| `sept 10`, `septem 10` | day | Any first three letters or more of a month name work, so `sep`, `sept` and `september` are all September. |
| `10/28`, `28/10` | day | Slash dates. Koru reads `10/28` as October 28; if only one of the two numbers can be a month it wins whichever way round you wrote it, so `28/10` is October 28 as well. |
| `10/28/2027`, `28/10/2027` | day | Month-first and day-first numeric dates put a four-digit year last. A two-digit year such as `10/28/27` is refused because `27` is also a day. |
| `aug 31 2027`, `aug 31, 2027` | day | An explicit year is taken as written, past or future. A date that does not exist (`feb 29 2027`) is refused. |
| any of the above + `9am`, `9:30pm` | time | Twelve-hour, with `am`/`pm`. `12am` is midnight and `12pm` is noon. |
| any of the above + `17:30` | time | Twenty-four hour needs the colon. A bare `9` is refused as ambiguous. |
| any of the above + `at 5pm` | time | An optional `at` before the time reads more naturally and changes nothing. |
| `5pm`, `9am`, `9:15pm`, `17:30` | time | A time **on its own** is today at that hour — or **tomorrow** at it, if it has already gone by. Typed at 10:15, `5pm` is this afternoon and `9am` is tomorrow morning. |
| `5 pm`, `9 am`, `friday 5 pm`, `aug 31 at 9 pm` | time | A space before `am`/`pm` is fine, and means exactly what the glued spelling means — anywhere a time may appear. |
| `30m`, `in 30m` | time | Minutes from now. `min`, `mins`, `minute`, `minutes` all work. |
| `4h`, `in 4h` | time | Hours from now. `hr`, `hrs`, `hour`, `hours` too. |
| `3d`, `in 3d` | day | Three days from now names a *day*, not this time of day three days on. `day`/`days` too. |
| `2w`, `in 2w` | day | Weeks. `wk`, `wks`, `week`, `weeks` too — and `2 weeks` with a space. |
| `2mo`, `in 2mo` | day | Months, keeping the day of the month where it exists: 31 January plus one month is 28 February. `mos`, `month`, `months` too. |
| `+2w`, `+3d`, `+4h` | as above | The same units, counted from **the day highlighted in the picker** instead of from now. |
| `-1w`, `-3d` | as above | And backwards, which has no other spelling. |

A duration with no sign, and the same one written with `in`, mean exactly the
same thing: counted from now.

The `/due` command currently uses the month-first numeric order above. Reminder
entry follows the device locale instead: year-first locales accept a complete
leading year as `2027-10-28` or `2027/10/28`. A year-first locale does not
reinterpret an ambiguous yearless pair such as `10/11` as US month/day; enter
the year or choose the date explicitly.

**`+` changed meaning, and this is a change rather than a new feature.** Until
now `+4h` was just another way of writing `4h` — the `+` was decoration and both
meant "four hours from this moment". It no longer is. `+` and `-` count from the
day the due picker is highlighting, which is what makes the phrase box able to
nudge the calendar (`+2w`, then `-3d`) and what makes going *backwards* sayable
at all. With the picker sitting on today the two spellings still agree, so you
will only notice once you have moved the selection — and typing `/due +4h` in
the command menu, where there is no calendar, still means four hours from now.
`in` and a sign contradict each other, so `in +2w` is refused rather than
half-understood.

**`4h` is a duration and `4pm` is a time**, and they do not collide: the unit
decides. `4h` is four hours from now; `4pm` is the hour named. A bare **number**
is neither — `31` is a day of a month, not thirty-one o'clock, so it is refused
rather than guessed at. That is the whole rule for a time standing on its own:
it needs an `am`/`pm` or a colon (`17:30`) before Koru will read it as a clock.
Writing the `am`/`pm` a space away does not change that, in either direction:
`5 pm` is five in the afternoon, and `5` on its own is still nothing.

What is **not** grammar (yet): `next friday`, `last tuesday`, `end of month`,
`eod`, ISO dates (`2026-08-31`), and a trailing period on an abbreviation
(`sept.`). Anything unrecognised is refused
rather than guessed at, so a due date is never something you did not say. The
words are English only for now.

`/attr` is the front door to all of it: it lists the attributes a block can
take (Tags, Color, Due and Priority) and opens the same choosers.
Right-clicking works too — the document's context menu has **Complete**,
**Tags…**, **Color…**, **Due…** and **Priority…** rows beside
*Move to document…*, and
they act on the whole block selection at once: set a due on five selected
rows and one `Ctrl+Z` takes all five back. In the sidebar, a document's `…`
menu carries an **Attributes** submenu that does the same for the page
itself — and so does putting the caret in the document's **name** and typing
one of the commands, which is the same attribute on the same thing by a
different door.

**One selection it will not act on.** A document's name and its body are two
different things to put an attribute on — the page, or these lines — so a
selection covering *both* has no single answer, and the four rows are dimmed
rather than guessing: they read **Title or body, not both**. Select the name on
its own, or the lines on their own, and they come back. (This is what the menu
used to do silently and wrongly: it applied to the lines and skipped the page.)

### Finishing a task

**A task with nothing under it is its checkbox.** Tick the box and the task is
done; untick it and it is open again. There is nothing else to learn, and the
two never disagree.

**A task with work under it is different.** Its checkbox is only a checkbox —
ticking it marks that line and says nothing about the steps beneath. Finishing
the whole thing is **Complete**, in the right-click menu: it ticks the box,
ticks every checkable line inside, and finishes any tasks nested below, all as
one change that one `Ctrl+Z` takes back. The same row reads **Reopen** on a task
that is already done, and reopening only reopens the task you asked about —
nothing below it is unticked. On a line that is not a task at all the row is
there and dimmed, so the menu keeps its shape.

Putting unfinished work back under a finished task **reopens it**, on the spot.
It does not matter how the work got there. Dragging a step in, indenting one
with `Tab`, pasting one — and **typing a new one**: press `Enter` at the end of
a finished task's last step and the new, unticked step reopens the task, because
there is now something in it that is not done. So does making a line inside it
into a task of its own, with `/task` or by giving it a date. That reopen is its
own `Ctrl+Z`: one undo finishes the task again and leaves your new line alone.

**The small orange triangle** ⚠ after a finished task means it is still holding
something unfinished — hover it and it says *Has unfinished items*. You get it
when two devices disagreed while one of them was offline: one finished the task,
the other added a step to it, and both are true. Nothing is rewritten behind
your back and no work is lost; the triangle is there so you can see it happened.
Tick the outstanding step off, or press **Reopen** and finish it properly, and
the triangle goes.

### Taking a row out of Tasks

Sometimes a line stops being something to do without ever being done. **Remove
from Tasks** is the last row of the right-click menu — red, under a hairline —
and it stops Koru tracking that line: it comes out of every task view, and its
**due date is cleared**.

Because the due date is the part that disappears without leaving a mark on the
page, Koru asks first, and the question names it: *"Cut the filter menu down to
a single control" is due Aug 30. Removing it clears the due date on the row in
More Documents; the row itself stays.* A task with no date is asked about
without that sentence — there is nothing to lose.

Three things it does **not** do. It does not delete the line: the text, and
everything under it, stays exactly where it was. It does not untick the
checkbox — check state is yours, not Koru's. And it does not clear the
priority, which is an ordinary attribute any block can carry. One `Ctrl+Z`
puts the task back with its due date, in one step.

**Enter means Cancel**, here as in every confirmation in Koru: the button that
takes something away is never the one a stray Enter presses. Reach it with the
pointer, or press Tab first.

On a line that is not a task the row is there and dimmed, the way **Complete**
above it is.

The due chip keeps its text short: **Today**, a weekday name for the coming
six days (**Thu**), then **Aug 31** — with the year only when it is not this
year's — and a 24-hour time appended for an exact moment (**Thu 17:30**), or a
**dash** on a task you have not given a date. It is the same chip throughout —
which is how you tell a task at a glance, because a task is the only thing
that wears one with nothing in it.
The priority chip is the level's name. Both are quiet neutral chips: an
overdue block is not tinted red, and a priority has no color of its own —
what a date *means* today is for views to say, later.

Tags belong to the **workspace**, not to a block or a document: the chip
shows the tag's current name, so renaming a tag renames it everywhere at
once, and two people applying the same tag are applying the same thing.
A tag can carry its own accent color, which is what tints its chip.

**Case and accents do not make a second tag.** `#HOME` typed where `#home`
exists is `#home` — and it stays spelled the way you first typed it, so a
shouted version of a tag never appears beside the quiet one. This works in
every language Koru ships, not just in ASCII: `#Café`, `#café` and `#cafe` are
all one tag, so are `#résumé` and `#resume`, so are `#STRASSE` and `#straße`,
and so are Turkish `#İş` and `#iş`. It is the same rule search uses when it
decides two spellings are the same word, so a tag can never split a search
result. The flip side is that you cannot keep `#resume` and `#résumé` as two
different tags — if you need the distinction, put it in a word rather than in
an accent. If two people each make `#home` while offline, the two records fold
into one the moment they meet, and both sides agree which one survived.

**Tags nest with `/`.** `#fun/home` and `#fun/outside` are two tags under
one branch, and `#fun` is a real tag of its own — typing `#fun/home` when
there is no `#fun` yet creates both, so you can colour the branch, rename it,
or put it on a block by itself. Renaming `#fun` to `#leisure` carries the
whole branch with it and touches none of your writing.

The chooser is aware of the shape: typing `fun` lists the branch and its
children, `fun/h` narrows to the children whose name starts with *h*, and
typing just `home` finds `#fun/home` without your having to remember where it
was filed. Each row shows the branch quietly ahead of the tag's own name.
On a block the chip shows only the **last** part — `#fun/home` reads `home`,
because the line is already about *fun*.

A tag path goes at most five levels deep, and the *Create tag* row will not
offer a name that starts or ends with `/`. Extra spaces around a `/` are
tidied away, so `#fun / home` and `#fun/home` are the same tag.

A block's attributes travel with it — moved, indented, synced — and applying
or clearing one is a single undo step.

## Getting things done

`Ctrl+Shift+Enter` ticks the box on the line the caret is in — a task, a
checklist item, anything that draws one — and the line **stays where it is**.
The order of your list is something you arranged; ticking something off is not a
request to rearrange it, and a row that jumps away as you tick it takes the
next one with it out from under the pointer. On a line with no box it does
nothing.

`Ctrl+Enter` ticks a **task** line, and means something else everywhere else:
in a paragraph or a table it opens a paragraph below, and in a code block it
leaves the block. That is why ticking has a key of its own — on a plain
checklist item the unshifted key writes a paragraph rather than a tick, so
until now the only way to tick one was the mouse.

Ticking the box is not the same as *completing* a task. A task with work nested
under it has a box that is only a box: **Complete**, on the line's right-click
menu, ticks it and everything checkable below it in one step.

`F4` hides completed tasks entirely, and shows them again — which is the way to
get them out of sight when you want that.

### The tasks rail

The third icon at the top of the explore column shows **your tasks across the
whole workspace**, wherever you wrote them: a task is still a row in a document,
and this is the index of them.

Five queues, each with the number of open tasks beside it:

| | |
| --- | --- |
| **Today** | due today, or already past. Its count reads `5 · 3 overdue` when some of it is late — the only urgent thing in the column |
| **Next** | due within the week, plus anything you marked High or Urgent with no date at all |
| **Upcoming** | further out than that |
| **Later** | everything else, including work with no date |
| **Completed** | below the hairline, dimmed. An archive, not a place you work |

Under **Upcoming** sits **Reminders**, with the bell's icon and its attention
count. It is a page, like the queues: pressing it points the Tasks tab at the
full *All Reminders* page and moves the selection highlight onto the row,
Ctrl+click opens the page in a new tab behind the one you are reading, and
right-click offers the queue's one-row menu, *Open in new tab*. The compact
popover from the caption bell stays as it was — same view, two hosts.

The numbers arrive shortly after you open the rail and refresh themselves while
it is showing; there is simply no number beside a queue until the first answer
comes back, rather than a `0` that would mean *we have not looked yet*.

**Lists are your own buckets.** They sit under the queues, each with a colour
square and the same count — and `1 overdue`, in the same warm colour, when
something in it is late. A task can be in Today *and* in Bike Project: a list is
one more thing that is true about a task, not a second place to keep it.

**`+ New list` makes one where it lives**: press it, or the `+` beside the Lists
heading, and one field opens in the column with a colour already chosen from the
set the rest of Koru uses. Type a name and press `Enter`. `Esc` closes it and
makes nothing; a name another list already has is refused where you typed it,
with the field still open. There is no dialog and no settings page — a list is a
name and a colour, and that is the whole of it.

**Right-click a list to change it.** Three things, because those are the three
things a list has:

| | |
| --- | --- |
| **Rename…** | opens the same field, over the row the list is already on, with the name selected. `Enter` keeps it, `Esc` leaves it alone |
| **Color** | the same nine colours the rest of Koru uses, by name. The square on the row changes, and so does the list's chip on every task in it |
| **Delete list…** | asks first, and says why: the list goes, and the tasks in it do not |

**Deleting a list never deletes a task.** The list leaves the column and every
task that was in it simply stops being in it — the row, its words, its due date
and everything else about it are untouched, and it carries on showing in
whichever queue its date puts it — with no list beside it, because there is no
longer a list for it to be in. `Enter` in that question means *Cancel*, as it
does in every question Koru asks before removing something: the answer that
deletes has to be chosen.

### The Tasks tab

Choosing a queue or a list opens **one Tasks tab** on it, and marks it in the
column. Choosing another queue points that same tab at it rather than opening a
second — the column has one selection, and two tabs would be two answers to the
same question. The tab survives a restart, on the queue you left it on.

The heading names the queue, with today's date beside it in **Today** and the
number of open tasks after that.

**A list is another bucket, not another screen.** Choosing one gives you this
same page: the same heading — the list's colour square, its name and its open
count — the same chips, the same rows. What changes is which tasks are on it,
and that tasks with no date are perfectly normal here, so their due chip is a
dash rather than a nudge to schedule something.

**The first row is where you add one.** Click *Add a task…*, type, and press
`Enter` — or, with the keyboard on the list's first task, press `↑` once more
and the caret lands in that row without you reaching for the mouse. `Esc` leaves
it again. A task with no date lands in **Later**; one typed while you are looking
at a list joins that list. If the last word or two is a date — `tomorrow`,
`friday`, `in 3 days`, `aug 31` — it is taken off the title and shown as the due
chip *before* you commit, so you can see what you are about to make. `Esc`
closes the row and makes nothing, and typing only a date word makes a task with
that name rather than an untitled one due then.

**Type `/` in the row for the rest.** `/due` opens the calendar, `/priority`
the four levels, `/task_list` your lists — the same three pickers a row's `…`
offers — and what you pick rides on the row as a chip beside the words until
you press Enter. A picked date outranks a typed one, so the word stays in the
title. The row is the same on every page: a queue, a list, Completed.

**What you just finished stays where you finished it, if you ask.** Ticking
a task takes it off the queue at once, which is the point — but sometimes you
want to see what you have done this morning without leaving the page. The
`…` at the top of any queue or list offers **Show recently completed**: turn
it on and anything finished today or yesterday is listed where its date puts
it, struck through, beside the work still to do. The number beside the queue
does not change, because it counts what is still open.

**Completed is a window, not an archive.** It lists what you finished
recently — the last month — so it stays a page you can read rather than one
that only grows. Nothing is deleted and nothing is hidden from the document:
a task that has fallen out of the window is still in its page, still ticked,
and still found by searching `/task_state completed`. To see the lot, search for
`/task_state completed`, which finds every finished task whatever its age.

Work you finished before this was built has no completion date recorded, so
it keeps showing until you reopen it, finish it again, or take it off Tasks.

**A task that lands somewhere else says so.** Type one due in three weeks
into Today and it is Upcoming's, not this page's: a toast says *Added to
Upcoming* with **View task** — the queue, with the row lit for a moment so
you see where it landed, and Back brings you here again — and **Undo**,
which takes it off Tasks. Filed in a list, the toast names the list and
*View task* opens it.

Press **Alt+3** to open the Tasks sidebar and focus its queues. Up/Down browse Today, Next, Upcoming, Later and Completed; Home/End reach the first or last queue. Enter or Space opens the focused queue, and Escape or Tab returns the keyboard to the page. From a Tasks page, **Ctrl+Shift+E** returns focus to the queues; press it again to close the sidebar. On a task in a document, that chord still reveals the task on its Tasks page. On Windows, assistive technology can also read and activate these five queues, including which one is selected. List creation and editing are separate controls and are not covered by this route.

Each row carries:

| | |
| --- | --- |
| the **circle** at its head | tick it to complete the task, wherever it lives and whatever kind of row it is |
| its **title** | with its own icon when it is an image, a file or a whole page |
| a **context line** | the document it lives in, folded in after the title — the same one line on every page. A task typed straight into this view has no page and shows nothing here; the missing page icon before its title is the reading |
| **chips** | its due date (a dash when it has none, warm when it is late), its priority, the list it belongs to, and *In trash* when its document has been binned |

**Clicking a row selects it** and leaves you on the page — you can run down a
queue reading it without losing your place. Everything you can do to a row is
in its `…` menu, or a right-click: *View in origin*, *Complete*, *Due…*,
*Priority…*, *List…*, and *Remove from Tasks*. *View in origin* takes you to the
document the task lives in, scrolled to that line, in a tab of its own, and
`Enter` on the selected row does the same. **Double-clicking a row opens its
origin in a new tab**, which leaves this page where it is — going back does
not return you to a queue and a row, so the second tab is the way back.
**`View in Tasks`** — on a due chip's menu, or `Ctrl+Shift+E` with the caret
on a task — opens the Tasks page on the queue the task falls in with its row
selected, and opens the column on the rail beside it. It is *View in origin*'s
return leg, and the two share one chord meaning: *show me where this lives*.
**`Due…` and `Priority…` act on the page**: the calendar opens over the row —
the same month grid, phrase box and *Clear* row the document's `/due` shows,
opened on the task's own day when it has one — and the priority picker offers
the four levels with *Clear* under them while one is set. Pick, and the row's
chip changes where you are; nothing opens and nothing moves. The calendar's
phrase box takes the same words `/due` does — `friday 17:30`, `+2w`, `in 3d` —
counted from the day the panel is showing. *Remove from Tasks* asks first, and
says what it does: it clears the due date and takes the row off this list —
**the row itself stays where you wrote it.**

**A page is up to half a minute old**, so a row you press may name a task that
somebody — you, in the document, or another of your devices — has since taken
out of Tasks. Koru does not quietly open the line underneath it: it says *This
task doesn't exist or was removed*, refreshes the page so the row goes, and
leaves you where you were. The same sentence answers a date or a priority
picked for a row that stopped being a task while the picker was open — a
deadline alone would have made it a task again, and a page of tasks must not
resurrect one you removed. The line is still in its document, unharmed; it is
just not a task any more, and following a task that no longer exists to a
paragraph nobody asked for would be the wrong kind of helpful.

**What you change here changes at once.** A task you add is on the page as
you press Enter, a tick takes the row off the queue, a new date moves it, and
the numbers beside the queues move with it. The store confirms a moment
later; the page and the rail keep asking it until its answer includes what
you did, and if a change is refused — the task was removed under you — the
row comes back the way it was and the toast says why. A task typed into the
page has no origin document, and its menu offers none; one that came from a
document wears a small page icon before its title, so the two kinds tell
apart at a glance.

**A row in a list leads there.** On Today or any other queue, a task that is
in one of your lists offers *Go to list* on its menu, and clicking the list's
chip on the row does the same: the page moves to the list, with the rail's
row selected. On the list's own page the chip is just the label it always
was. The date and priority chips are labels everywhere — their changes go
through *Due…* and *Priority…*. Back returns to the queue you came from.

**Following a task lights it up.** Enter, a double click or *View in origin*
opens the task's document with its line flashed for a moment, the way a
search result is, so you can see which paragraph you were sent to.

**A task you typed here can be given a page.** Its menu offers *Create
document* in place of *View in origin*: a new page named *Notes* and the
task's words appears at the top level of the workspace, and **the page is
the task** — its title wears the due, the priority and the list the row had,
and the caret waits on the empty line under it for whatever you want to
write. The row here reads the page's name from then on, shows the page's
icon, and *View in origin* takes you to it. Back, or closing the page's tab,
returns you to the list. To finish the page, type `/completed` in its title:
the title strikes through and the task moves to Completed; `/completed`
again reopens it.

**`List…` puts the task in one of your lists**, without going anywhere: a small
chooser opens over the row with your lists in it, the one it is already in
ticked, and *Clear* at the bottom to take it out again. A task can only be in
one list at a time, so choosing another moves it. The list's name then rides on
the row as one more chip — which is how a task is in Today *and* in Bike
Project without you having to keep it in two places. If you have not made a list
yet the chooser says so, and the sidebar is where one is made.

A completed task is dimmed and struck through. One still holding unfinished work
underneath it wears a warning triangle, the same mark the document view uses.

**Upcoming groups its rows by week near and by month far**, under a quiet rule:
*This week*, *Next week*, then *Later in September* over everything falling in
that month. The rules are there while the list is in date order — sort it by
priority or by when it changed and they go, because a heading that named a week
would then appear halfway down the page again.

**And back again.** In a document, right-click a task row — or any of its
chips — for **Go to task**: it opens the Tasks page on the queue that task
falls in, with the row selected.

**The tab is named for the page it is showing** — *Today*, *Next*, or the
list's own name with its colour beside it — because one Tasks tab serves every
queue and every list in turn, and the strip is where a window you are not
looking at tells you where you were. `Ctrl+Shift+E` on that tab opens the
column on the tasks rail with the same queue or list selected.

**One tab, unless you ask for another.** Pressing a queue in the rail re-points
the Tasks tab you are on. **Ctrl+click**, or right-click → *Open in new tab*,
parks it in a second tab instead and leaves you where you are — which is how
you keep Today in front of you while you go through Later.

### Narrowing what you are looking at

Under the heading is a row of **chips**, and each one is a filter the store
applies — the list you get back is the list that matched, not a shortened copy
of the one before it.

| | |
| --- | --- |
| **Priority ≥ High** | only work marked High or Urgent |
| **Any date** | on a **list**, *when*: one chip you press through its answers — *Any date*, *Overdue*, *This week*, and back. A list covers all of time, so which stretch of it you are looking at is worth saying. A queue is already a stretch of time, so it does not offer this at all: *This week* over **Next** would be this week's work inside this week's work, and *Overdue* over a queue that starts today can never match anything |
| **Open** | on a **list**, whether you are looking at open work, completed work, or both. A queue already says which — Today is open work and Completed is the archive — so the chip is only offered where it can change something |

A chip that is on **inverts** — dark, with an `×` — and the rest stay grey; they
combine, so on a list *Priority ≥ High* and *Overdue* together is the late work
that matters. Press an inverted chip again to switch it off. A filter you set
on a list is cleared when you leave it, so nothing narrows a queue without a
chip on screen saying so.

The **`…`** at the top right holds the three things that change what comes back
rather than how it is arranged:

| | |
| --- | --- |
| **Show completed inline** | completed tasks among the open ones. On a queue this is dimmed, and says why: a queue is open work, and Completed is its own queue |
| **Show removed to trash** | tasks sitting in a document's bin, each wearing an *In trash* chip. Off unless you ask |
| **Sort** | *Due date*, *Priority* or *Modified* |

**The menu stays open** while you change these, so you can see a check move and
change a second one without opening it again. `Esc` closes it, and so does a
click anywhere else.

An **Updating…** chip appears beside the `…` when one of your workspaces is
still catching up on its index. Nothing is hidden while it is there: the rows
you are reading are real, and the chip only says another few may still arrive.

### When a queue is empty

An empty page says one line, on the row where the first task would be — never a
picture in the middle of the screen.

| | |
| --- | --- |
| **Nothing due today.** | this queue is empty. **Add a task** sits at the end of the line and puts the caret in the row above it |
| **Nothing in this list yet.** | the same thing on a list |
| **Nothing completed yet.** | the archive, which offers nothing to add — a completed task arrives here by being completed |
| **· Checking your workspaces…** | one of your workspaces has not finished being read yet. It is worded away from the others on purpose: a zero mid-sweep is not the same as having nothing |

If the store could not be reached, the line says so instead — that one you can
act on, and the others would be a guess.

### The keyboard on this page

`↑` and `↓` move down the rows; from nothing, `↓` takes the first and `↑` the
last. `Enter` goes to the selected row's origin, which is what the menu's first
row does. The **menu key** (or `Shift+F10`) opens that row's `…` menu where the
button is.
`Esc` lets the selection go. Clicking a row moves the selection to it, so what
you press next acts on the row you just touched.

While you are typing in the capture row it keeps the keyboard: `Enter` commits
and `Esc` closes it.

## Reminders

Reminder preferences belong to the current Profile. Linked devices exchange
them directly when both peers are available, even without Koru Cloud. An open
Settings → Reminders screen refreshes when another device changes those
preferences.

**End of day time** accepts a local `HH:MM` time; leave it empty or use **Clear
time** to keep the profile default unset. **First day of week** controls what a
reminder phrase such as `next week` means: **System default** follows this
device's regional calendar, while **Monday** and **Sunday** are explicit
profile-synchronized overrides. `next week` resolves to 9:00 AM on the first
day of the following week. **Default tone** is a dropdown of the operating-system
sound and ten Koru tones; the play button beside it plays the chosen tone
once (Windows). **System default** for the first day of the week names the day
it means on this computer, and **End of day time** is a 24-hour time such as
`17:30`. These profile defaults synchronize with the profile.

**Deliver on this device** controls only the computer where you change it.
Switching it off withdraws this device's pending native notifications without
deleting reminder rules; switching it on repairs the native schedule from the
synchronized rules. Going offline, losing permission, or losing the Agent does
not change the switch you chose. Other computers and phones are not controlled
from this row.

The notification row shows the operating system authorization reading. Only
the explicit **Allow notifications** action may show a permission prompt;
refresh, repair, scheduling, and **Test notification** never prompt. The test
uses real native delivery and creates no reminder. If notification settings or
the native schedule need attention, Settings offers the direct system-settings
or repair action.

**Reminder health** is a factual panel, not a score. It reports whether this
device is enabled, the next scheduled reminder, the last successful scheduling
synchronization, and one actionable failure without examining reminder text.
When the first reminder needs Koru to start at sign-in, Settings asks once and
records either answer; declining leaves the reminder active. Mobile reminder
delivery remains phase 2.

Type `/remind` on a task, document, or other object, or choose **Add reminder**
from its context menu to open the reminder popover. You can enter a natural-language time, choose **Later today**
(at 6:00 PM, before that time), **Tomorrow** at 9:00 AM, or **Custom**, or open
**More options** for explicit date and time fields. Nothing is saved until you
choose **Set reminder**. Escape or a press
outside cancels the unsubmitted popover.
Partial or invalid date/time values keep **Set reminder** unavailable. A past
time is never silently accepted: Koru shows a proposed future time for explicit
confirmation. If **end of day** has not been configured, Koru asks you to set
it. Select that request to open Settings → Reminders with **End of day time**
focused and scrolled into view. Ambiguous timezone abbreviations are not
guessed. Numeric dates use the
current locale's order, punctuation, and digits, and the clock follows the
operating system's 12/24-hour preference. Compact reminder confirmations,
chips, and list rows show time in the current device zone; a reminder created
with an explicit zone labels that display zone, and its confirmation also names
the canonical authored zone when they differ. The authored zone name appears
below its structured fields, which show and resolve that zone's wall time even
after the device changes zones. Canadian English and French dates use
year-month-day; other supported regional formats likewise keep their local
order and punctuation. In those Canadian locales, `/remind 2027-10-28` is a
valid complete numeric date; an ambiguous yearless pair is not guessed as a
US month/day date.

At/before-due reminders keep their
relative meaning and are edited through the natural-language field in phase
one; the shown date/time is only a preview.
Minute- and hour-relative phrases such as `in 1 min` land on the minute shown by
the interface: seconds are zeroed at the reminder boundary. `/due` retains its
exact elapsed-time behavior.

Tab and arrow keys move focus through visible controls. Enter or Space activates
the focused control; typing changes the natural-language phrase only while that
field is focused.

**Ctrl+Shift+R** opens the reminders on the item your cursor is in: the one
already saved, so you can read, change or delete it, or a new one when there is
none. It is the keyboard equivalent of selecting the item's reminder pill, and
it always shows what storage came back with — not what you typed, and not the
pill that appeared while the save was still going.

Submission removes only that command's exact text and immediately shows its
inline reminder pill, which remains visible while saving completes. Several
reminders may save independently. A failed save shows recovery controls for
**Retry**, **Edit**, and **Dismiss**; dismissing them does not restore command text.

That recovery pill floats near the top-right and belongs to no panel, so
**`F6`** is how you reach it without a mouse: `F6` moves focus into the window's
chrome, and a standing recovery pill is the first thing it lands on. `Tab` and
the arrows move across **Retry**, **Edit** and **Dismiss** — across every pill,
if more than one save failed — `Enter` or `Space` chooses one, and `Esc` leaves
the pill without recovering anything, which puts you back where you were typing.
Pressing `F6` again moves on to the caption bell, its other stop; with nothing
failed, `F6` goes straight there as it always did.

An object shows one inline bell pill per reminder occurrence, immediately after its
text and other metadata pills. Several reminders remain separate rather than
collapsing to a count. A paused reminder shows a pause icon instead of a bell.
A snoozed reminder shows a bell-with-`z` icon and the absolute date and time at
which that occurrence will be delivered again, rather than its original time.
A handled occurrence remains visible with its schedule struck through.
On a task, a small red dot on the checkbox means one of its reminders has
fired on this device and is still unhandled. Upcoming, snoozed, paused, and
handled reminders do not show that attention dot.
When a full pill does not fit inside the document, it moves intact to the next
line; its schedule is not cut off. Right-aligned text keeps reminder lines at
the document's right edge. Select a pill to edit that exact reminder. Add another
from the object's **Add reminder** context-menu action. Right-click a live
reminder pill for that reminder's **Edit**, **Pause** or **Resume**, and
**Delete** actions. A handled pill offers only **Edit** and **Delete** because
there is no live occurrence left to pause. The document formatting toolbar stays closed. Saving an edit closes
the reminder editor. Pause, resume, delete, handled history, and history
clearing also remain available in **All Reminders**.
If loading or an update fails, reminder changes are disabled and offer
**Retry** or **Discard**.

The caption bell shows reminders delivered on this device that still need
attention; counts above 99 display as **99+**. Open it to find **All Reminders**
first, the next future delivery (including a snoozed occurrence when it is
earliest), then recent fired-but-unhandled reminders
with Open, Snooze, and Handled actions. Its height never exceeds 400 pixels.
Handling one row removes it in place and decrements the badge; the popover and
the other fired rows remain where they are.
Choose All Reminders for the profile-wide view inside the same bell popover; use the
left-arrow control beside All Reminders to return to reminder attention without
closing the popover. The filters are content-sized chips that flow to another
line when necessary. Snooze offers minus and plus controls for 1, 5, 10, 15, and 30 minutes
and 1 hour. Its preview shows the accumulated duration and final date/time; the
chosen instant is fixed when you confirm. The view filters All,
Needs attention, Coming up, Snoozed, Paused, and History; All leaves handled
history out. Rows open their exact source and offer the actions valid for their
state. Pause and Resume follow the reminder rule itself, including when an
unavailable source makes an enabled reminder appear effectively Paused. Clear
all history appears only in History and requires confirmation.
The Reminders row in the Tasks sidebar opens the same view as a full page in
the Tasks tab; the Agent tray and a missed-reminder summary open the bell's
compact presentation of it.

On a Windows notification, **Snooze** opens Koru at that exact reminder and
shows the same duration chooser; nothing is snoozed until you confirm a time.
**Handled** immediately handles only that occurrence, withdraws its native and
bell attention, keeps it in History, and leaves its inline schedule struck
through. **Open** and dismissing the Windows notification do not handle it.
If Agent restarts while Koru remains open, the window reconnects and registers
its current profile and workspace again. The bell then refreshes from Agent's
current reminder snapshot, and later Windows actions continue to open their
exact source rather than falling back to a generic Koru launch.

## When undo says no

`Ctrl+Z` reverses your own work, and only your own. If a change has arrived from
another window or another peer since, the underlying store will sometimes refuse
rather than risk unpicking somebody else's edit — and the status bar says so
instead of leaving you pressing a key that does nothing. Carry on; the message
clears itself the moment you do anything else.

Your work is written out when you pause for a moment, and at least every few
seconds while you are typing without one. Closing the window writes it too.

## Appearance

Light and dark are a **welcome page** preference under Preferences:

| | |
| --- | --- |
| **System** (default) | Match Windows; follow live when Personalisation changes |
| **Light** / **Dark** | Sticky; ignore OS theme changes until you pick System again |

The status bar does not show the theme. There is no keyboard shortcut for it.

**Caret colour** is four swatches on the same Preferences section (default
neutral, blue, amber, violet). Each is tuned for light and dark, and the text
**selection** tint follows that colour. **Caret width** is the row under them:
Thin, Med, Thick, 4×, 8×. **Caret height** is the next row: Short, Med, Tall
(Med is the default letter-height scale). In italic text the caret slants with
the letters. All of these are yours only — documents never store them.

| | |
| --- | --- |
| `F7` | documents only — cycle the page layout: Page → Narrow → Full |
| `Ctrl+Shift+P` / `N` / `G` | documents only — Page, Narrow or Full width outright |
| `Ctrl+Shift+T` | pin the window above other windows (per-window; the title-bar menu toggles it too) |
| `Ctrl+=` / `Ctrl+-` | zoom in / out (or `Ctrl` + scroll); `Ctrl+0` resets |

## The title bar menu

The top-right keeps direct **minimize**, **maximize/restore**, and **close**
controls. The leftmost control is an overflow `…` menu. Move the pointer onto it
or click it to open application destinations:

| | |
| --- | --- |
| 📌 Always on Top | `Ctrl+Shift+T` |
| Settings | `Ctrl+,` |
| Zoom | placeholder |
| Feedback | Send feedback tab (general, idea, or bug) |
| Help / Keyboard shortcuts / Mobile | placeholders |
| Updates | opens update management |

Pinned, the 📌 draws solid rather than outlined, so "on" is visible at a glance.
Opening the menu never changes the overflow glyph into a close button; clicking
it again dismisses the menu. Walk straight down into the panel to reach a row,
or press `Esc` to close it.

**Double-click the empty part of the title bar** — anywhere past the tabs — to
maximise, and again to bring it back. Dragging that same strip moves the window;
a maximised window has to be restored first, which is what the double-click is
for.

`Ctrl` while clicking the direct close control hides Koru behind the Koru Agent
icon instead of quitting it. The plain close control still closes the window.

A standalone document window puts the document's **name in its title bar**
rather than as the first line of the page. `F2` renames it from there — it works
even with the window menu open, since that menu is about the window and this is
about the document in it.

Nothing is lost by closing a window: your work is written continuously, and the
window comes back with its tabs.

**The Settings tab** (`Ctrl+,`) is a list of pages down its left —
**Appearance**, **Editor**, **Reminders**, **Terminal** and **System**, the
current one marked by an accent rule — with a search box above them that
narrows the list by what a page holds (type *microphone* and only Editor is
left; Enter opens it). In a narrow window or split pane the list becomes a row
of chips above the page, so the page keeps its full width. Each page groups its settings into cards
of rows: the setting's name on the left, one control on the right. A choice
among a few short words is a segmented control; a longer list — fonts,
languages, microphones, tones, the renderer — is a dropdown showing the value
in force, opened with a click or Enter and stepped with the arrow keys.

**Fonts and density** are on the **Appearance** page: a density control
(Compact / Normal / Comfortable) and four font dropdowns — the **app** font
(tabs, sidebar, pills, panels), and three document roles, **body**,
**headings** and **code**. Each dropdown lists every family set in that
family's own face, so you see what you are choosing. The theme is chosen from
three preview tiles at the top of the same page. A document's page menu
also has a **Typography** group whose Body / Headings / Code rows cycle a
role's family for a quick in-document switch. Twelve families span sans, serif,
rounded and mono; code defaults to JetBrains Mono.

The four roles are separate on purpose. Setting the app font to a monospace
makes every *pill* monospaced and leaves the sentence around it alone. All four
are remembered and come back when you reopen the app.

**Zoom** (`Ctrl+=`, `Ctrl+-`, `Ctrl+0`, or `Ctrl` + scroll) scales the whole
window — text and chrome together — and rewraps the content to fit, between
50% and 200%. A percentage flashes centred as the value changes, and the level
is remembered across restarts. It is `Ctrl+=` rather than `Ctrl+Plus` because
the latter is `Ctrl+Shift+=`, which is already superscript.

**Every choice on this tab saves as you make it.** There is no Apply button and
never was one; what is new is that a second Koru window open beside this one
follows along within a moment, rather than the two disagreeing until one of them
was closed. The same is true of Koru AI's window and Koru Terminal, which take
the theme, the caret and the fonts from the same place.

The **Spelling** card on the **Editor** page controls spell checking and its
language. When the dictionary for that language is not on this computer, a
**Dictionary** row says so, with its size, and **Download** fetches it —
checked against a fixed fingerprint before it is installed — and spell
checking starts without a restart; if the download fails the row says so and
offers **Try again**. Words added
with **Add to dictionary** belong to the active Profile and follow it to linked
devices. **Clear personal dictionary** asks for confirmation, then removes all
of those added words from that Profile on every linked device. The language's
base spelling files remain local application resources and are never synced;
when the selected files are missing, Settings says that spell checking cannot
run on this device instead of silently presenting an enabled but inactive
checker.

**Two settings belong to this computer rather than to you**, and they sit on
the **System** page (the renderer under *Advanced*). Neither follows you to
another machine, because neither would be true there.

**Renderer** picks the graphics backend Koru draws with — **Auto**,
**DirectX 12**, **Vulkan** or **OpenGL** — and applies the next time Koru
starts, which the row says. Auto is right for almost everyone; the others are
for a machine whose driver misbehaves. Nothing can be broken by choosing wrong:
a backend that will not start is reported and Koru falls back to the one that
works.

**When the last window closes** decides what ✕ does when it is the only
window left. **Keep in background** is the default — Koru stays ready in the
notification area, so global search still answers and opening it again is
instant instead of a fresh start. **Ask** puts the question each time, and
**Quit** closes Koru outright. If the Koru Agent is not running there is no way
to bring Koru back, so ✕ closes it whatever this says; and a window closing for
an update always closes properly.

**Voice input** is the last card on the **Editor** page, marked *Transcribed
on this device* — the point of the whole feature: voice is transcribed on this
device, and audio is not uploaded or retained. That is true because of how it is built
rather than because of a promise — the microphone is opened by a separate small
program that sends back only the words, so the sound never leaves it.

**Language** is *Detect automatically* by default, or one of eight languages
named in its own language. Detecting is the right answer for almost everybody;
naming one helps when you switch languages mid-sentence and would rather not
be guessed at.

**Microphone** is which microphone Koru listens to. **System default** is the
starting point and it means *follow whatever the operating system is using* —
change headsets and Koru comes with you. Choosing a particular microphone
instead pins it by name, so it is still the same one after a reboot or after
something else is plugged into a different socket.

The notification-area **Input** submenu lists the same microphones. Once voice
models are installed, Agent prepares the idle voice helper in the background so
that list is ready; preparing it neither loads a speech model nor opens a
microphone.

If the microphone you chose is not connected — a headset that is asleep, a
sound interface that is unplugged — Koru does not refuse to listen and does not
quietly change your mind. It uses the system default and says so, both here and
in the notification area: *Yeti Nano is not connected. Using the system default
until it is.* Plug it back in and it is used again without you touching
anything.

**Quality** is **Compact** or **High accuracy**, and the line under the row says
what the choice actually does rather than only what it is called. The
difference is not only the size of the download: with Compact the words appear
when you finish speaking, because that model transcribes a finished sentence;
with High accuracy they appear as you speak and are revised as you go. Compact
is the default and the lighter runtime; High accuracy is a bigger model that a
modest laptop may find slow. Setup installs both once, so changing this
preference later never stops to download another model.

**Shortcut** shows the chord that starts a dictation, and lets you choose what
it does. **Hold to talk** is the default — press and keep holding, and it stops
when you let go. **Toggle** starts on one press and stops on the next, which is
easier for a long dictation and for anybody who would rather not hold a key
down.

**Model** says whether both speech models are on this computer, and how big they
are together. If either is missing, **Install…** offers the remaining bundle —
about 532 MB for a first setup — and names the combined size, both licences and
both attributions before downloading. Progress identifies model 1 of 2 and
model 2 of 2 and can be stopped; a model that already completed is retained, so
retrying resumes with only the missing tier. If the download
cannot be offered at all, the message says which of the reasons it was:
*Could not reach the model catalog* is your connection, *the model catalog is
not published at the address this build uses* is the build or a
`KORU_VOICE_CATALOG` you set, and *the model catalog could not be read* means
something answered with a file this build does not understand. **Remove** deletes
both; you can install them again afterwards without being asked to agree a
second time, because agreeing and having the bytes are two different things.

Language, quality, the microphone and the gesture **follow you** — they are
yours, not this computer's, and they arrive on any machine you sign in to. The
microphone is remembered by *name* for that reason: a machine that does not
have it falls back to the system default and says so, rather than silently
listening through something else. Whether a model is
installed is a fact about *this* computer, and it is read from the disk each
time rather than remembered, so deleting the cache is noticed rather than
believed.

If the section says voice input is unavailable, one of two things is true: it
has been switched off for this installation, or the program that does the
listening will not start. It says which.

**Nothing is left running that you cannot see.** Koru's three programs are the
window, the **Koru Agent** in the notification area, and the **Koru Store**
that holds your writing; the last two have no window of their own. Quitting the
agent stops the store as well, unless a window still needs it — and in that
case the store stops when you close the last window instead. A window you had
kept in the background closes itself if the agent it was waiting behind goes
away. Whatever order you quit things in, closing the visible ones leaves
nothing behind.

## Shells and agents

Koru runs terminal shells and coding agents as tabs, right beside your
documents. Press **+** at the end of the tab strip (or **Ctrl+Shift+A**) and
pick **Shell** or **Agents**. **Active** lists running sessions of that kind
you can switch to; below it are the same new-profile rows as before
(System, PowerShell, Bash; Claude, OpenCode, Codex, Grok, Kimi). The session
opens as its own tab and starts running; an agent you have not installed is
shown greyed out with *Not installed*.

The left navigator's fourth mode — the robot icon, beside Documents, Files
and Tasks — is **Agents**: every running session, grouped by project. Click one to open or
focus its tab. Use the **+** on the Projects heading to make a project;
right-click a project for **Shell / Agents** to start a session inside
it, or to rename or delete it; drag a session onto a project to move it.
Right-click a session for **Open in new tab**, **Move to new window**, **Move
to project**, or **Terminate**.

A session belongs to your machine, not to the tab. **Closing a tab leaves the
session running** — reopen it from the Agents list. Typing `exit` (or Ctrl+C)
ends the shell itself, and its tab closes. Close Koru and reopen it and your
session tabs come back, reattached, as long as the background Koru Agent kept
running; if it was stopped, the tabs return saying the session ended, and you
start fresh. The Koru Agent's tray menu has the same **Agents** list, so you
can jump to a session from there too. The composer (for sending a message to a
managed agent) is hidden until you need it — **Ctrl+Shift+U** shows it.

### Handing a piece of work over

Delegating is asking an agent to go and do something while you get on with
something else. There are five ways to start one, and all five open the same
window:

- **The + menu** at the end of the tab strip, the row called **Delegate…** —
  when you have no particular page in mind.
- **`/delegate`** in a document. Type `/` at the start of a word, pick
  **Delegate** (or type enough of it), and the row you typed on comes along as
  context. The typed `/delegate` is removed before the window opens, so
  cancelling leaves your page exactly as it was — and **anything else on that
  line becomes the objective**, so writing what you want done and then typing
  `/delegate` at the end of it is the whole gesture.
- **Right-click a task** in the Tasks tab and choose **Delegate…**. The task's
  title becomes the first line of the objective, and the task's own row is
  attached.
- **Right-click inside a document** and choose **Delegate…**, beside *Add
  reminder*. The row you clicked on is attached, and its words become the
  objective.
- **Re-run**, on a run that has ended — see *Asking for the same work again*
  below.

The window first asks **When**. **Run once** is selected by default and keeps
the ordinary delegation path below. **Repeat** creates a schedule instead of a
run. Nothing runs at the moment the schedule is created. When Repeat began
from a document or task, Koru leaves one schedule pill on that originating
row; the `+` menu has no document and therefore leaves none. Each later firing
creates its own separately named run without adding another schedule pill.

For a repeating delegation, choose **Hourly**, **Daily**, **Weekly** or
**Monthly**. Weekly rules offer Monday-through-Sunday chips, while daily,
weekly and monthly rules take a time in the format this device normally uses.
Monthly rules also take a day of the month. **This device’s local time** keeps
the wall-clock time with whichever device owns the schedule; **Fixed time
zone** keeps it in the named IANA zone. The sentence below the controls is the
rule Koru will store.

**Advanced cron** is another editor for that same rule, not a second setting.
It accepts Koru’s supported five-field cron forms. In addition to the picker’s
forms, `*/5 * * * *` runs every five minutes and `5,30,45 * * * *` runs at
those three minutes of every hour. The standard `* * * * *` form runs every
minute and is rewritten as `*/1 * * * *` when you leave the field. Minute
steps and lists require `*` in all four remaining fields. A valid picker-shaped
value updates the picker; an interval or list leaves all four picker chips
unselected because none of them represents it. Choosing a chip deliberately
returns to a simple rule. The advanced value is rewritten in canonical form
when you leave the field. An
invalid partial stays in the field so you can correct it, leaves the last valid
rule intact, says what needs correcting, and disables **Create schedule**.
Invalid times, monthly days outside 1–31, and invalid IANA zone names behave
the same way. Monthly days accept and display the current locale's digits.

The rest of the window asks the same four things for either choice.

**Objective** — what the agent should do, in your own words. It is several
lines: Enter starts a new one, and **Ctrl+Enter** hands the work over.

**Agent** — which agent runs it. Registered managed agents remain in the
list when unavailable: Checking, Not installed, Signed out, Unsupported, or
Check failed. Focus or click a row to read what to do next, then use its
**Retry** button after fixing the problem. An agent that can run managed
sessions but cannot safely accept delegated work says **Cannot delegate**;
choose another agent. Unavailable rows can be inspected but cannot be selected
or submitted. The list updates while the window stays open and keeps your
objective and attachments. A plain terminal-only agent is absent from this
picker; its ordinary **+ > Agents** row still follows its own installed check.

Kimi 0.42.0 and OpenCode 1.18.27 are available for experimental delegation
after those checks pass. Completion, approvals and cancellation are ready to
try; their native child-agent behavior remains unverified. If a run stalls or
fails, report the provider version, the action you attempted, and the visible
failure without including document content or credentials.

**Model** appears for agents that expose model choices; OpenCode is the first.
Its default is initially selected. Search models from providers connected in
OpenCode, browse the provider groups, and select the one to use. Automatically
detected cloud credentials and free providers without an explicit connection
do not appear in this chooser. Changing the working folder refreshes
the catalog; **Refresh models** checks it again after an OpenCode configuration
change. An unavailable explicit choice must be replaced or deliberately reset
to the default before submission. Changing the agent clears the previous
agent's choice.

The run's **Overview** shows **Requested model** when you chose one. **Re-run**
preselects it, and resuming retains it. Model discovery does not send the task;
OpenCode must accept the model before the first prompt. A startup refusal keeps
the reason visible instead of silently using another model. **Activity** and
**Overview** show the failed stage, a recorded cause and recovery guidance when
available. Older failures with no recorded cause retain the stage-only message.

**Tab** and the arrow keys reach unavailable rows and Retry. If the content
does not fit the window, the wheel or scrollbar reaches every row and the
buttons; keyboard focus scrolls into view automatically.

**Working folder** — where the agent works. The list is your projects (the
same ones the Agents navigator groups sessions under) with, at the end, *a
folder Koru makes for this run* for work that belongs to no project. **Every
row says which folder it means**, under its name: a project shows its own
folder, and the last row shows the folder Koru makes each run's directory
inside. A project with no folder set shows no path and is greyed out saying
*No folder set*.

Koru's own folder starts inside this computer's temporary directory —
`%TMP%\koru-runs` on Windows, `/tmp/koru-runs` elsewhere — and each run gets
a directory of its own named after it, with the agent's working folder inside
that. **Change…** on that row opens a folder chooser, and what you pick is
remembered on this computer for every run after it. Koru checks the chosen
folder in the background and keeps the previous folder until the check succeeds.
If it cannot use the chosen folder, it says so and keeps the one you had.
Choosing again replaces a pending choice; a slower earlier check cannot replace
the newer one.
(Your projects' folders are not Koru's to move; those are set on the project.)

**Context** — what the agent is given. Whatever the gesture attached is listed,
and you can take any of it away with **Remove**. When the tab behind the window
is showing a page that is not already in the list, **Attach the open document**
adds it. For **Run once**, what is attached is captured the moment you press
Delegate: editing the page afterwards does not change what the run was handed,
and neither does deleting it. For **Repeat**, the schedule keeps live pointers
to those documents and rows. Each firing captures what they contain then; a
source that no longer exists pauses the schedule visibly instead of silently
using an old copy.

Below those, **What the agent may reach** says in one sentence what this run
will be allowed to open — the documents in this workspace and the results of
earlier runs in it — with a **Remove** beside it. Take it away and the run
reaches only what you attached above, and nothing else in the workspace.

If you opened this window with **Re-run**, everything above arrives filled in
from the run you started from — and every bit of it can be changed before you
press Delegate. If something that run was given has since been moved to the
Trash or deleted, it is left out and the window says so.

**Tab** moves between every control and **Esc** cancels. For **Run once**,
pressing **Delegate** sends the request; the window then says it has been
delegated and names the run. If another run is already working in the folder
you chose, it says so and names it — both will run, which is your call to make.
For **Repeat**, a required **Name** appears and is filled once from the first
line of the current objective. Editing either field after that does not change
the other. The name must be one non-empty line of at most 128 characters.
Pressing **Create schedule** stores the named recurring intent and
the window says **Schedule created**; there is no run yet to name or open. If
Koru cannot create either one it says so with the reason. Close the window
while it is still sending and the answer arrives as a notice instead. Until
that answer arrives, starting another delegation shows the current progress
instead of replacing the request that is still being created. A
schedule error offers **Try again**, which replays the same request and
Schedule identity so an answer lost after creation cannot turn one press into
two schedules. A Run once error is Close-only: start it again from the form so
its context is captured once, at that new press, under a new Run identity.

While a create or Edit form is still being filled in, clicking the gray area
outside the panel closes it without saving. The outside click is consumed and
does not activate the document underneath.

The schedule pill uses the schedule name and clock icon. Renaming the schedule
updates its label; pausing it replaces the clock with a pause icon, and
resuming restores the clock. Choose the pill to open the Agents column with
that schedule selected; it remains selected even though the document is still
the active tab. Right-click it for the same Rename, Edit, Pause or Resume, and
Delete menu as the schedule row. Rename edits the pill in place when the Agents
column is not visible.

### Finding scheduled work

Open the **Agents** column (`Ctrl+4`) and look under **Schedules**, beside the
Projects and Runs sections. Each schedule is labeled by the name you gave it.
Open its fold to see its rule in ordinary words — for example, *Every weekday
at 09:00* — when it will fire next, which device owns it, and whether
it is active, paused, or waiting for an ownership handoff.

Runs that a schedule actually started sit under that schedule. Choose one to
open the same run view you would reach from the Runs section. A busy or missed
occurrence is recorded as a skip and does not pretend that a run exists, so it
adds no child row. A schedule with no firings simply has no run children.

Point at a schedule or any of its rule/next/owner/state details and press `F2`,
or right-click and choose **Rename…**, to edit its name in place. `Enter`
saves and `Esc` cancels. Empty names, names over 128 characters, and control
characters are refused beside the field. A fired Run child remains a Run and
the Schedules heading remains a heading; neither can rename its parent.
Schedules owned by another device show where they must be managed instead of
pretending the action can work locally. Choose **Edit…** to reopen the same
delegation form in fixed Repeat mode. It starts with the schedule's complete
stored request: name, objective, agent, model, working folder, context, reach,
rule, and time-zone choice. Those exact choices stay selected and **Save
changes** remains available if their provider, model catalog, or folder is
temporarily unavailable; a schedule that cannot fire pauses visibly instead.
Saving affects only future firings; runs already made keep what they were
given. Cancel or a refusal changes nothing.
If another update made the form stale, Koru asks you to close it, review the
current schedule, and try again instead of combining the two edits.
An active schedule's menu offers **Pause**. This stops future firings but does
not cancel a Run that was already started. Any paused schedule offers
**Resume**, whether it was paused by you or because a provider, folder, or
context was unavailable. Resume starts with the next future occurrence; it
does not make up occurrences that passed while paused. If the unavailable
thing is still unavailable, the next firing can pause the schedule again.
Choose **Delete…** to stop the schedule permanently. The confirmation defaults
to **Cancel** and explains that future Runs stop, existing Runs remain, and the
schedule cannot be restored. Confirming removes the schedule row; its existing
Run children remain available under Runs with their original provenance. It
does not put anything in Trash.

Schedule parent rows can be selected together with Ctrl-click or a Shift
range. Run and Schedule selections remain separate. A same-state local
selection offers **Pause** when every schedule is active or **Resume** when
every schedule is paused, plus **Delete…** for the complete set. Mixed active
and paused selections offer Delete only; Rename and Edit remain single-
schedule actions. Bulk Delete asks once, defaults to Cancel, stops future Runs
for every confirmed schedule, and preserves all Runs already made.

If only part of the
catalog could be read, Koru keeps the schedules it did receive on screen and
says that the list is incomplete. Loading, failure, and a genuinely empty list
are also named separately.

### Following a run you handed to an agent

You handed a piece of work to an agent and walked away. **The Agents column
(`Ctrl+4`) lists the runs of the workspace you are in**, under *Runs*,
newest first, named by what you asked for. Choose one and it opens in a tab
with four views:
newest first.

**Every run has a name of its own** — two words, like *sunny-otter* or
*copper-wren*. It is not something you type: Koru works it out from the run
itself, so the same run is called the same thing on every one of your
machines, and you can say it out loud to somebody. It is the name in the
runs list, on the run's tab, and at the top of the run's views. What you
actually *asked for* is on the Overview, where it has room to be read.

Choose a run and it opens in a tab with three views:

- **Overview** — where it got to and what it is waiting for, the machine
  running it, what you asked for, the working folder, everything you gave it
  and whether each of those is still there, and what it cost: time actually
  working, tokens, steps, tool calls and how often you stepped in. A number
  nothing reported reads *not reported* rather than nought.
- **Activity** — everything it did, newest first, one sentence a step. You
  should be able to work out what happened without opening the terminal; if
  you cannot, that is a bug worth reporting. A run started by a schedule has
  one provenance line above those steps saying that it was a scheduled firing,
  with the schedule's stable id, the exact occurrence time in your local date
  and clock format, and the schedule revision that fired. That line remains
  when Activity is empty and when you load earlier steps. A direct **Run once**
  has no schedule line. The line describes this historical firing; it does not
  reinterpret it through a recurrence rule you may have edited since.
  **A step where tool calls failed says which ones, and why.** Under
  *6 tool calls, 2 of them failed* there is a line per kind of failure —
  *Running a command — reported an error, 2 times*, *An MCP server's tool —
  was not allowed to run*. The tool is named by what kind of thing it was
  rather than by the agent's own name for it, and the reason is one of five:
  it was not allowed to run, it was stopped before it finished, it ran out of
  time, it reported an error, or *failed, and the agent did not say how* —
  which is what you get when the agent's answer was not something Koru could
  make sense of. **Koru will not guess, and it will not quote.** What the
  command was and what it printed stay in the session; the run's history
  never holds them. Runs from before this was built show the counts on their
  own, with nothing under them.
- **Result** — what it handed back. A run that finished without registering
  anything says so; it never reads as a delivery. **Press a result to open
  it**, and it opens **as its own tab**, the way every other document in
  Koru opens — named after the result, so you can find it in the strip, put
  it beside something else, and keep it open after you are done with the
  run. The run stays on its Result view, exactly where you left it. If the
  result is Markdown — and an agent's answer usually is — it opens **in
  Koru's own document view**, the same one your own pages open in: real
  headings, real bullets, real links, code in code type. It opens
  **read-only**. You can select text and copy it, and `Ctrl+F` finds inside
  it, but you cannot type into it, and it is **not** added to your library —
  it is a page made from the result and thrown away when you close the tab.
  Anything that is not Markdown is named instead: what kind of file it is,
  how big, and what it was called. A result whose content is on another
  machine says which one, and says it without pretending to fetch something
  it cannot reach; one with nothing in it says that rather than showing you
  a blank page. Pressing the same result again brings you back to the tab
  that is already open on it rather than opening a second one.

  **To change one, press Edit.** The page is replaced by the result's
  Markdown in a box you can type in, and the control below reads **Save as
  version 2** (or 3, or 9) with a line saying that the version you opened
  stays exactly as it is — saving a result adds a version, it never rewrites
  one. **Stop editing** puts the page back. The version history is in the
  **Artifacts** column.
- **System prompt** — the instructions every delegated agent is given before
  your objective, so you can read what the agent was told without going
  looking for the file. They are shown **as a document**, because that is
  what the file is. The file it comes from is
  `docs\prompts\delegated-agent.md` beside `koru.exe`, and the tab names it.
  **It shows the instructions as they stand now, not a copy of the ones this
  particular run was handed** — the tab says so in its first line — so if you
  edited the file after this run started, what you are reading is your edit.
  It also says whether the text is your file or Koru's built-in copy, and if
  an edit of yours could not be used it says that here too, with the reason.

  **You can change them here.** **Edit** opens the instructions in a box you
  can type in; **Save** writes them back to the file above, and every run you
  start after that reads the new text. Save stays dark until you have changed
  something, and a save that fails says so and leaves the file exactly as it
  was. If Koru's built-in copy is the one in force there is nothing to edit
  and nothing to save — the tab says so, and offers neither. What you type is
  what gets saved: Koru writes the document back in its own Markdown, which
  is the same text the tab was showing you.

  The note in `<!-- ... -->` at the top of the shipped file is shown in
  lighter type. It is a message to whoever edits the file rather than an
  instruction to the agent — but the agent does receive it, so Koru does not
  hide it.

**Taking the session, and ending the run, are different things.**

- **Take the session** waits for the agent to finish the step it is on, then
  hands you the session. **End the step and take the session** does not
  wait — the step it is working on stops now. *Both of them leave the run
  running.* When the session comes to you it opens as an ordinary session
  tab: type in it as you would in any other.
- **Give the session back** hands it to the agent, which carries on with
  what you asked for.
- **End the run** is the other thing entirely. The run is over: it cannot be
  resumed, and whatever it had not finished will not be produced. It is the
  red one, it sits on its own at the bottom, and it asks first — and that
  question has no default answer, so pressing Enter does nothing until you
  choose a button.

Every one of those says what it would do in a line underneath it, before you
press it.

#### Watching the agent work

**A run's session sits underneath the run in the Agents column**, not down in
*Unassigned* with the sessions you started yourself. Click it and the agent's
chat opens and keeps moving as it works.

**It is read-only, and it says so.** There is no cursor and nowhere to type,
because the agent has the session — a line at the bottom of the chat tells you
that rather than leaving you wondering why the keyboard does nothing.

At the foot of the chat there is one button: **Take the session**. Pressing it
asks the agent for the session at the end of the step it is on — exactly the
same as asking from the run's Overview, and, like that one, **it does not end
the run**. While you are waiting the button goes and the line says the session
comes to you when the agent finishes its step. When it does, the same tab
becomes an ordinary session you can type in; give it back and it goes quietly
back to being something you are watching.

**Rating a run** is one press — good or not good — on a run that is over.
It is never asked for and never in the way; an unrated run is perfectly
normal. You can add a note afterwards if you want to; the note stays with
the run and is never counted or sent anywhere.

**On another device** you can follow the whole run — status, activity,
result, the machine running it — and there is nothing to press. Only the
device actually running the work can act on it.
#### The pill it leaves behind

Delegating leaves a small pill in the document you started from, so the line
you were working on remembers that you handed it to an agent. Where it lands
depends on where you started:

- **`/delegate`** and **Delegate… in a document** put it at the end of the row
  you were on, after your words. Nothing you typed is changed.
- **Delegate… on a task** puts it on the task's own row, so the task itself
  tells you it has been handed over.
- **The + menu** leaves none — it was not about any page, so there is nowhere
  in your writing it belongs. Those runs are found from the runs list.

The pill says **Delegated run** for a moment, then starts saying how the run is
going: *Starting…*, *Working…*, *Needs you* when the agent is waiting on an
approval or an answer, and then *Finished*, *Failed* or *Stopped*. **On any
device other than the one doing the work, it also names that device** — for
example *Working… · Studio* — so you always know where the run actually is.

**It stays after the run is over.** A finished run's pill still says *Finished*
months later, and clicking it still finds the run: it is a record of who did
what, not a progress bar that disappears. Deleting the line the pill is on
never deletes the run — removing a run is its own action.

A pill Koru cannot resolve yet — a run on a device you have not synced with —
simply reads *Delegated run* rather than looking broken, because the run is
real and somewhere else.



#### Asking for the same work again

When a run is over — **Finished**, **Ended** or **Failed** — there is a
**Re-run** button on that line. Press it and the ordinary delegation window
opens filled in from that run: the same objective, the same agent, the same
working folder, the same attachments and the same reach. Change whatever you
like and press **Delegate**, and Koru starts a **new** run. The old one is not
touched, and closing the window without pressing Delegate changes nothing at
all.

Two things are worth knowing. The attachments come back as the *pages
themselves*, not as the versions the first run was handed — asking for the
same work on the same page means the page as it is now. And if something that
run was given has since been moved to the Trash or deleted, it is left out and
the window says so, because a run cannot be started with material that is not
there.

**Re-run is not Resume.** Resume continues *the same run* and only ever
appears on a run that was interrupted; Re-run appears only once a run has
ended, and makes a separate one. You will never see both on the same run.

### The instructions a delegated agent starts with

When you hand a piece of work to an agent to do on its own (delegated runs,
being built), the agent reads a short set of instructions before your
objective: where the context you attached is, where to put its results, that a
result only counts once it has been registered, and that it must not start
agents of its own. Those instructions are a plain text file you can open and
edit: `docs\prompts\delegated-agent.md` in the folder Koru is installed in,
beside `koru.exe` (on a Mac, inside the app under `Contents/Resources/docs/`).
Edit it in any text editor and save it as UTF-8; the next run you start uses
what you wrote, exactly as written — Koru does not check or correct it. The
only times it falls back to its built-in copy are when the file is missing,
empty, cannot be read, is not UTF-8 text (an editor that saved it as UTF-16,
say), or is larger than 64 KiB — and for everything except *missing*, the run
tells you the file was not used and why. Delete the file to go back to the
built-in instructions; an update puts the shipped copy back beside the others.

**To see what the instructions currently say**, open any run and choose its
**System prompt** view: it shows the text that is in force, whether it came
from your file or from Koru's built-in copy, and where the file lives. You do
not have to restart Koru after editing — the next run you start reads the file
again.

**You can also edit them from there**, without leaving Koru: press **Edit**,
change the text, press **Save**. Koru writes the file above and the next run
reads it. Two things it will refuse to save, because a run would refuse them
too: instructions with no text in them, and instructions over 64 KiB. Either
way nothing is written and the file stays as it was.

## What a delegated agent handed back

The left navigator's last mode — the tray icon, after Agents, or
**Alt+6** — is **Artifacts**: everything the delegated runs in this
workspace produced, newest first.

Each primary row is the latest **result** — the thing you named, or the thing
an agent named when it saved one — with a file icon and version suffix, such
as **Dad Joke - 1.0**. Click that row to open the artifact. If it has older
versions, use the small caret to reveal them; the latest version is not repeated
underneath. Click an older version and Koru writes out where it came from, in
words:

> Version 3 came from run 9f2c11ab, 2 hours ago.
> It was built on version 2.
> Run 41d0e7c2 used it as input.

That is the whole of provenance — no diagram to read. A result with a long
history shows a screenful of older versions and an **N earlier versions** line;
click it for the next page. Opening a result never gets slower as it
accumulates versions.

### Reading a version, and revising one

The primary row opens directly. Under an older version's provenance sentence,
**Open** does the same. The artifact opens as a tab and the Artifacts column
stays on its list. If the version is Markdown, Koru draws it the way it draws
any document; anything else is named rather than drawn — what it is, how big,
what it was called — and a version whose content is on another machine says
so.

When its producing Run still exists, the artifact tab has a robot icon
immediately left of **Edit**. Click it to open that Run. A Run in Trash is still
reachable; after it is permanently deleted, the icon disappears while the
artifact remains readable. A version registered by hand has no Run icon.

**Edit** opens the same document page for typing, and the control becomes
**Save as version 4** — with the number it would actually make. The line below
says the rest out loud: *this makes version 4; version 3 stays exactly as it
is.* That is not a caution, it is how results work. **A version is never
changed.** Saving adds one to the family, and the new version's own provenance
sentence says it was built on the one you started from, so the history of a
result reads as the history of your thinking about it.

### Files the agent wrote but never handed back

An agent saves its work in a folder of its own and then **registers** what it
means to give you. Anything it wrote and did not register is not a result —
and that is the most common way a run disappoints you, so Koru shows it
rather than hiding it.

Under **Not results yet**, grouped by the run that wrote them, are those
files. Each has a **Register** at the right of its row: click it, and the
file becomes a result and moves up into the list above. One click, no
questions — the name it had is the name it gets, and registering the same
file twice does not make two of them.

A run that finished without registering anything says so under its heading,
and tells you how many files it left you can register. If it left none
either, it says that too — the run genuinely produced nothing, and that is
worth knowing rather than guessing at.

If Koru will not register a file, it says why on the line underneath: the
file changed while it was being registered (write it fully, then try again),
it is no longer inside the run's results folder, the folder itself is gone,
or the result it belongs to has been retired and takes no new version. Click
**Register** again once you have dealt with it.

### A result whose content is on another computer

Results are kept on the machine that made them. If you are looking at one
whose content is not on this computer, the row is dimmed and the line under
it says the content is elsewhere — with the machine's name when Koru knows
it. You still see what it is, how big it is and when it was made; you cannot
read it here until Koru's syncing of results ships.

You can walk the whole column with the keyboard: **↓** and **↑** move,
**→** reveals older versions and steps into them, **←** closes the history,
**Enter** opens the primary artifact or chooses the older-version action you
are on — including Register — and **Esc** hands the keyboard back to what you
were writing.

## Canvases

A **canvas** is a surface you lay cards and pictures out on, rather than a page
you write down — and where you build a small chain of agents that writes a page
for you.

- **Making one.** **+ → Canvases → New canvas**, or **New canvas** in the empty
  Canvases section. It is made the moment you choose it — unlike a new page,
  which waits for your first keystroke — at the top level of the workspace,
  untitled, and opens in a tab of its own.
- **Finding one.** The Explore column's seventh icon, or **Alt+7**, shows
  **Canvases**: every canvas in the workspace in one flat list. Canvases hold no
  pages, so nothing nests there. The Documents section lists pages only. The
  `+` menu's Canvases flyout lists your recent canvases under **New canvas**.
- **Opening, moving and binning one.** Click its row to open it; drag the row
  onto a pane to open it there, or onto **Trash** to bin it. The list is not
  reordered by dragging — the order is the order they were made. A canvas tab
  wears the four-squares icon, comes back after a restart like any tab, and can
  be pinned; it does not pop out into a window of its own yet.
- **Renaming one.** `F2`, the tab's **Rename…**, or **Rename…** on its row: the
  same name box a page has. The new name reaches the tab, Explore, the recents
  and search the way a page's does.
- **Moving around.** Drag empty space to pan; the wheel scrolls (`Shift` +
  wheel sideways). `Ctrl` + scroll, a touchpad pinch, and `Ctrl+=` / `Ctrl+-` /
  `Ctrl+0` zoom the canvas around the pointer — from 10 % to 400 % — and leave
  the window's zoom alone. A second `Ctrl+0` at 100 % fits everything in view.
- **Cards.** Double-click empty space, press **Add card** on an empty canvas,
  or press `Enter` with nothing selected: a small dialog asks for the card's
  text. `Enter` starts a new line; `Ctrl+Enter` or **Save** makes the card
  where you double-clicked; `Esc` or **Cancel** makes nothing. Double-click a
  card (or select it and press `Enter`) to change its text the same way.
- **Arranging.** Drag a card to move it; drag its right edge to change its
  width. The arrow keys nudge the selected card (`Shift` for ten pixels) and
  pan when nothing is selected; `Tab` walks the cards. `Delete` deletes the
  selection. Every gesture is one `Ctrl+Z`; `Ctrl+Y` or `Ctrl+Shift+Z` brings
  it back. `Esc`, or switching away from the window, cancels a drag in flight.
- **The right-click menu** (or `Shift+F10`, or the Menu key) adds a card, a
  **start node**, an **agent node** or an **output node**, or inserts an image;
  over a card it edits, connects, restacks or deletes it.
- **Arrows.** Drag from the small circle on a card's right edge onto another
  card, or choose **Connect to…** and click the card to connect to.
- **A chain of agents.** Add a start node, one or more agent nodes and an
  output node, and connect them in that order. An agent node asks for its
  instruction when you add it, and starts with the agent you last delegated
  to; its header opens a menu of the agents ready on this computer. An output
  node asks for the title of the page it will write. Press **Run** on the start
  node to run the chain; each node shows where it is, and **Stop** stops it.
  When the page is written, its title appears on the output node — click it to
  open the page. **Place on canvas** copies an agent's result out as a card.
- **Answering an agent.** When an agent needs approval or a reply, its card
  shows **Needs you** and **Respond…**, with **Stop** still available. Click
  **Respond…**, or select the card and choose it from `Shift+F10`. Koru opens
  the run and requests control at the agent's pause, then opens its session
  with the question, approval choices and reply box. Answer there. On the run
  tab, choose **Give back control** so the chain can continue. If the question
  has already cleared, the run opens without taking control. Runs on another
  device retain the run view's guidance about where they are executing.
- **Pictures.** Drop image files on a canvas, paste one with `Ctrl+V`, or
  choose **Insert image…** from the right-click menu. Pasting *text* onto a
  canvas does not make a card yet.

## Documents

Type `@` and start writing a document title. The text beside the caret filters
the shared search index while the popover lists every matching document as a
separate row. It searches every active workspace by default; archived and
deleted workspaces stay out of both the filter and its results. Use the workspace
filter to narrow the scope, then pick a result to insert the pill. **Clicking that
pill opens the document.** The default list shows recent documents and always
offers **Add new document**. With a typed query, that action creates the page with
the query as its title, replaces the authored `@query` with its pill, and opens
the new page.

**Type `#` to link to a heading rather than a whole page.** With a document
highlighted, `#` swaps the list for that document's headings, indented by level;
what you type after the `#` filters them. Choosing one makes a pill reading
`Page#Heading`, and clicking it opens the page scrolled to that heading with the
row briefly highlighted. Backspacing over the `#` goes back to the document
list, on the row you left. `@#` with nothing typed before it offers the headings
of the page you are already in, which is how you link to a section further down.

If a page has no headings the list says so, and pressing `Enter` there makes an
ordinary link to the page — nothing is lost by having tried. A heading you typed
a moment ago may take a second to appear, and the list says *Indexing…* while it
does.

The caption follows both halves: rename the page or retype the heading, from any
window, and the pill catches up within a couple of seconds.

If the linked page later moves to Trash, its pill changes from the page icon to
a trash icon. The pill remains clickable: it opens that workspace's Trash view
on the deleted page so its contents can still be reviewed.

**Move a selection to another document with the same `@`.** Select whole rows
(Shift+↓, or drag in the gutter) and type `@` — or right-click the selection
and choose **Move to document…** (a text selection within one line works too).
The same picker opens, with its own search box — already holding the text your
selection started with (the words you selected before extending across blocks,
or the first selected line), so a document with that name surfaces at once;
type to replace it. Pick an existing document and the selection moves to the
**top** of it; type `#` to place it **under a heading** instead; or choose
**Add new document** to create the destination named by the box and open it —
or press **Ctrl+Enter** to move without following: the pill lands, the new
document waits in the sidebar, and you stay where you were. **Shift+Enter**
also stays, and creates the new document as a **child** of the one you are
on — a sub-page. With nothing
typed, the list shows your recently opened documents. Extending a text
selection across blocks and shrinking it back also returns the exact text
selection you started with. Where the selection was, a document pill remains — the trail back — and for
a heading it deep-links there. The content is written to the destination, and
confirmed on disk, *before* anything leaves the source: if the move fails
half-way you may find a copy at the destination, never a loss. `Ctrl+Z` in the
source brings the moved content back (the destination keeps its copy — undo
does not cross documents). Moving works across workspaces; `Esc` abandons the
move untouched.

Matching is the same as global search: it is case- and accent-insensitive,
multi-word queries require every term, exact and prefix matches rank first, and
longer terms allow the same limited typo correction. The picker constrains those
results to live document titles. If several documents are called `Hello`, each
has its own row and path; choosing one stores that row's stable document id, not
the text `Hello`.

A document is the same editor you already know. Every line is a row, `Enter`
makes the next one, and all the formatting works. What differs is the furniture:
there is almost none. No toolbar — the page starts at the top of the window —
and no checkboxes, because a document's lines are paragraphs rather than tasks.

**The first line is the document's name.** Click it and type: it is an ordinary
line, so you can select it and undo in it. It takes no **formatting** — a bold
half-title is something a tab label and a tree row would draw as plain text
anyway — so no bar appears over a selection there. **A code block is the same**,
for a different reason: its colours come from the language, not from marks you
put on it. It is *not* an ordinary
line for anything structural — `Enter` in it moves down into the body rather
than splitting the document in two, and it cannot be deleted, dragged or
indented. Renaming it renames the document everywhere: every pill pointing at
it, every tab, every window.

**It does take an emoji**, all three ways: `:smile`, `Ctrl+Shift+Space`, and
`/emoji`. An emoji is a
character, so a name carrying one is still a name. **A leading emoji becomes the
page icon** in the library and the tab strip, and is not repeated in those
labels — so `💻 Pricing` shows a laptop icon beside "Pricing", not the document
glyph and then another laptop. An emoji mid-title stays in the name. The F2 page
icon still applies when the title has no leading emoji. `@` stays refused for
the same reason bold is: a contact is a *pill*, anchored to a character, and a
name is drawn as plain text in every pill pointing at it, so it would show as a
stray glyph there.

**It also takes the document's own attributes.** `/attr`, `/tag`, `/color`,
`/due`, `/priority` and `/task` all work with the caret in the name, and what
they set belongs to the **document** — the same thing the **Attributes** submenu
on the sidebar's `…` menu writes, from the other side. So `/` on that line
offers seven things: those six and `/emoji`. **`/task` on the name makes the
page itself a task** — the page is the task's content, and it is completed
explicitly rather than by a checkbox, because a page has none. The chips appear after the name, exactly as
they do after any other line — and they are clickable there in the same way, so
a document's due date or tag can be changed by pressing its own chip. (A `#`
typed mid-name is still just a `#`: the
tag shorthand stays out of the title, where the name is short and every word
is part of it.)

**Skin tones.** The emoji picker shows one cell per emoji (default tone). A
short click inserts your preferred tone when the emoji supports one. **Hold** on
a tone-capable emoji in the picker — or on one already in the document — to open
a strip of variants; picking one inserts or **replaces** it and remembers that
tone for next time until you change it again. Selecting only emoji does not open
the formatting bubble (marks do not apply). The preference lives with you
(`preferences.conf`), not in the document.

**The tab and the tree catch up when you leave the line**, not as you type. A
tab that changed width under the pointer, and a tree that re-sorted itself while
you were looking at it, for a name half written, was worse than a moment's wait.
The document itself is saved on every keystroke as always — it is only the two
lists of names that hold on.

### Find and replace in this document

Press `Ctrl+F` to open a small search panel at the top-right of the current
document. It aligns to the page edge in Page or Narrow layout and to the window
edge in Full width. The query is focused immediately. If text is selected it
becomes the query; otherwise the tab remembers its last query and matching
options. Search covers the visible title, prose, lists, tasks, quotes, code and
rendered table-cell text. It does not search pictures, collapsed rows, or the
labels and destinations inside atomic pills.

Matches update as you type and use the selected caret colour. The active match
briefly grows by 2 px and darkens, and the document scrolls just enough to show
it. Use `Up` / `Shift+Enter` for the previous match and `Down` / `Enter` for the
next; the arrow buttons do the same. Stepping beyond either end wraps and says
**back to top**. **Match case** and **Whole words only** are under `Aa`; this
panel and Replace are mutually exclusive.

Choose Replace, press `Ctrl+H`, or use `Ctrl+Alt+F` when the keyboard layout
delivers that chord. (`Ctrl+Alt` can be AltGr on Windows, so `Ctrl+H` is the
reliable shortcut.) Pressing `Ctrl+H` again hides Replace. `Enter` in the
replacement field replaces the active match and advances; `Shift+Enter`
replaces it and moves backward. On an empty replacement field, `Backspace` or
`Shift+Tab` returns to Find and hides Replace. With Replace open, `Tab` from
Find moves to the replacement field and selects any replacement already there.
`Ctrl+Enter` replaces every current match. Replace All closes the panel,
selects the inserted text at the current match, and shows **Replaced X
matches — Undo**. Undo is checked against the current text first, so a peer's
intervening edit is never partly overwritten.

Each single Replace is its own ordinary document undo step. After the panel is
closed, successive `Ctrl+Z` presses follow those replacements backwards: each
restored word takes focus with the caret immediately before it.

`Esc`, Close, or a click outside closes local search and leaves the active match
selected. An outside click still performs its underlying action. Reopening the
tab's search remembers the query and options but starts with Replace collapsed.

To add a picture, type `/image` on its own line and pick one off disk, drag a
file onto the window, or **copy image(s) anywhere and press `Ctrl+V`**. A paste
lands each picture as its own row directly below the line you are on — several
at once if you copied several files — and each one shows up as a numbered child
(`Image 1`, `Image 2`, …) of the document in the explore column. The bytes live
in the workspace — not the line — so they sync as a blob, not as text.

A picture whose bytes have not reached this device yet shows a placeholder
that names a known holder — *"Waiting for Studio to come online."* The message
wraps inside the image box. If Koru has not yet received trusted device or blob
origin information, it says it is waiting for a device with the image rather
than guessing a name. Open Koru on a device that has it and the picture fills
in by itself.

A paste can mix the two. Copy a Word document with paragraphs and inline
pictures and `Ctrl+V`: the paragraphs come in as text, and the pictures are
added as image blocks after them, in picture order. (Vector drawings from Word
do not come through — only photos and other raster pictures.)

Click a picture to select it: it wears a thin border with a box at each corner.
Drag any edge to resize — the top and bottom edges change the height, the left
and right the width — and drag a corner to change both. Hold `Shift` to keep the
aspect ratio; without it you can stretch the picture freely. A `W×H` label sits
over it while you drag.

The selected picture is a block. Drag its six-dot handle on the left, or drag
the picture itself, to move it above or below another block. Press `Enter` to
move into the following block; if the picture is last, Koru creates an empty
paragraph below it. Double-click the picture or its row in Explore to rename it;
unnamed pictures keep their generated `Image N` label.

A selected picture answers the alignment keys (`Ctrl+L`/`E`/`R`) the same
way prose does — left, centre or right within the column — and `Ctrl+C` copies
it to the clipboard as an image, ready to paste into another app. `Ctrl+X` does
the same then removes the picture from the page. Selecting several rows that
mix text and pictures copies them together: HTML Format carries every picture
inline, and a paste back into Koru restores the same order.

### Code blocks

Type `` ``` `` — three backticks — at the start of a line, then `Space` or
`Enter`, and the line becomes a code block: the fence text disappears and the
block renders in a mono font on a tinted panel, ready to type into. Name a
language straight after the backticks — `` ```rust `` then `Space` or
`Enter` — and the block keeps it; a pasted fence works the same way. The
fence waits for that last key so the language can be typed first.
`Ctrl+` `` ` `` toggles the focused line between a paragraph and a code
block, and `/code` converts without pinning a language.

Inside a code block, `Enter` starts a new line **in the same block** rather
than a new row — a code block is one block that holds many lines. To **leave**
the block, press `Ctrl+Enter` (move to the next paragraph, or make one), or
press `Enter` on the trailing
blank line (the "type your code, then `Enter` twice" idiom). Backspace at the
start of an interior line joins it with the line above.

Hover a code block and a small **language chip** appears at its corner —
`rust`, `python`, or `Auto` when none is set — with a **copy** icon to its
left. Click the icon to copy the block's source; a *Copied to Clipboard*
toast confirms it. Click the chip to pick a grammar (or `Auto` to clear it); the list ends
with **Mermaid**, which turns the block into a diagram — and turns a
diagram that was switched to a grammar back into one.
The dropdown opens with the chip's current value already
active, entries highlight as the pointer crosses them, and `Enter` confirms
the active one. Syntax color is derived from the text and the language;
it is never stored, so it follows the theme and never bloats a sync.

### Diagrams

A code block whose language is **Mermaid** is a diagram: Koru draws it in
place of the text. Write a flowchart or a sequence diagram in
[Mermaid](https://mermaid.js.org) notation inside a fence that opens with
`` ```mermaid `` — or paste a Markdown document that holds one, or let an
agent write one over MCP — and the page shows the drawing, in the page's own
colors and font, in light and dark alike. The text is the truth and the
drawing is derived from it: it is never stored or synced, so every device
draws its own, and a page with a diagram opens as fast as one with a picture.

Koru draws **flowcharts** (`flowchart` or `graph`, every direction, groups
included), **sequence diagrams** (participants, actors, every arrow style,
notes, activations, and the `loop`, `alt`/`else` and `opt` frames) and
**pie charts** (`pie`, with its `title`, the percent on each slice and a
legend beside; `showData` puts the values in the legend). Every
other Mermaid kind keeps its text and says so beneath it: *Koru does not draw
this kind of diagram yet*. Styling and interaction lines in the text —
`%%{init}`, `classDef`, `style`, `click` — are kept exactly as written and
ignored for drawing; the theme decides how a diagram looks.

**Starting one.** Type `/diagram` — or `/mermaid` — or choose *Diagram*
from a row's `+`. The block arrives with a small three-box example already
drawn and its text open with the example selected: type or paste your own
over it, or press an arrow to keep it and edit. On an empty line the line
becomes the diagram; on a line with words, the words stay and the diagram
goes below them. Typing `` ```mermaid `` and `Enter` (or `Space`) on an empty
line starts an empty one.

**Editing.** Click a drawing to select it, like a picture; arrow onto it and
the *Edit text* control appears at its corner. Press `Enter` (or click the
control, or choose *Edit text* from the right-click menu) and the text opens
above the drawing, which follows every keystroke. When a line cannot be read,
the block says which one — *Line 7: this line could not be read* — and keeps
the last drawing that worked beneath, labelled as such, until the text draws
again. `Esc` closes the text, and so does the check at the block's corner,
left of the copy icon; `Ctrl+Enter` leaves to a paragraph below. A
block that does not draw shows its text with the reason under it, never a
stale drawing. A very large diagram shows its text with *Too large to draw:
N boxes* (or *N messages*) rather than slow the page. An empty block shows
*Describe a flowchart or a sequence diagram here*.

**Looking closer.** Rest the pointer on a drawing and a small bar appears
at the block's corner with four controls: a magnifier, *Copy image*, *Save
as…* and *Edit text*. Click the magnifier — or double-click the drawing, or choose *Open*
from the right-click menu — and the drawing opens in its own view over the
page, at its natural size when that fits and fitted to the window when it
does not. Zoom with the `−` and `+` at the bottom, `Ctrl`+wheel, `Ctrl+=`
and `Ctrl+-`, or a pinch; `Ctrl+0` returns to natural size and the fit
control shows the whole drawing again. A drawing larger than the window is
scrolled with the wheel — `Shift`+wheel sideways, `Alt`+wheel up and
down, whichever way the wheel turned — stepped with the arrow keys, or
dragged, and never hides under the toolbar. The view takes the whole
window, sidebar and all, so a large drawing is seen as large as the window
allows. The view's copy control puts a picture on the
clipboard and its save control saves one; `Esc`, the `×`, or a click
anywhere on the dimmed window closes it.

**Copying and saving.** Copy on a selected drawing copies the whole block;
it arrives as a diagram wherever it is pasted in Koru, and as a fenced
`mermaid` block in other applications. *Copy image* in the right-click
menu — or the copy control that appears on the drawing, or `Ctrl+C` in the
drawing's own view — puts a picture of the drawing on the clipboard
instead, at twice the screen size, ready to paste into a slide or a chat;
the control shows a check while it does, the way every copy in Koru does.
Right-click a drawing and choose *Save as…* — or press `Ctrl+Shift+X` on
it, or click the save control beside the copy one — to save the picture as **PNG**
(at twice the screen size) or **SVG**; both look like the drawing on screen
on any machine, and the SVG needs no font. On Windows the dialog offers the
two types; on macOS the extension you type decides (`.svg` for SVG,
anything else for PNG).

**Screen readers** hear a drawn diagram as a picture named with what it
shows: the title and description the text declares (`accTitle`, `accDescr`),
then *Diagram, flowchart, 10 boxes, 12 arrows* and every arrow as a sentence,
or *Diagram, sequence, 4 participants: …; 12 messages* and every message. A
block that does not draw is announced as *Diagram, not drawn:* with the
reason, and its text is read as usual.

Agents connected over MCP learn how to write a diagram and whether it drew
from the model document (`koru://docs/model`, section *Diagrams*): every
read of a Mermaid block carries a `diagram` status with the same reason a
person sees.

### Inline code

For a span of code inside a sentence, select the text and press `` ` `` (the
backtick key), or press `Ctrl+Shift+C` (the `</>` button on the selection
bubble). The span renders in the same mono font and tinted background as a code
block. Press either again on an all-code selection to clear it.

### Formatting: select some words

Nothing is on screen until you select something. **Select some text and a small
dark bar appears just above it**, carrying everything you might do to those
words:

```
┌───────────────────────────────────────────────┐
│ Text ⌄ │ B  I  U  S │ ▤ │ </> │ Link │ T⊘ │
└───────────────────────────────────────────┘
```

Left to right: what the line *is* (a dropdown of **Text** and the four heading
levels), then bold, italic, underline and strikethrough, highlight, inline code,
a link, clear formatting, and the three alignments. (A `⋯` used to end the bar
holding super- and subscript; it is off the bubble until those two commands
work.) Every button has a keyboard chord — hovering one tells you what it
is, and the menus list theirs beside each row. Press `Esc`, or click away, and
the bar goes.

**It waits for you to let go.** Selecting is a drag, and a bar that turned up
the instant you had one character — then moved, then flickered, then vanished as
the drag crossed into the next paragraph — was in the way of the thing you were
doing. It appears the moment you release.

**`Ctrl+.` summons it without selecting anything**, at the caret. That is the
only way to reach the `Text ⌄` dropdown and the three alignments from the
keyboard — neither acts on a *range*, so neither is reachable through the
selection that otherwise brings the bar up. The formatting buttons work too, and
they do there what `Ctrl+B` does at the same caret: arm the next thing you type.
It stays put while you type under it and goes when you press `Esc`, click
elsewhere, or move the caret to another line. Summoned over a document's name or
inside a code block, nothing appears — neither takes formatting.

There is no undo button: `Ctrl+Z` and `Ctrl+Shift+Z`.

### Adding a block: the `+` in the margin

Hover any line and two small marks appear in the left margin: a **`+`** and the
six-dot drag handle you already know.

```
 +⠿  The product replaces Google Docs → Slack → Trello.
```

Click the `+` for the same menu `/` opens — a heading, a picture, a code block,
a divider, a task list, a document reference. The new block lands **below** the line
you hovered; hold `Alt` and it lands above. Nothing is created until you pick
something, so pressing `Esc` leaves the page exactly as it was.

**The menu opens with a search box, and the caret is already in it**, so just
start typing to narrow it down: `div` for a divider, `head` for the headings,
`list` for the list styles. What you type goes into the box rather than into
your page, so backing out of it leaves nothing behind. The same box is there
when you type `/`. Arrow up and down through the entries and `Enter` takes the
highlighted one; the wheel scrolls the list without moving what is highlighted,
the way scrolling anything else does.

### The page's own settings

The **`…`** to the left of the document's name opens them:

```
PAGE SETTINGS
  ▭  Page             Ctrl+Shift+P  ✓
      Color                ● ○ ○ ○
  ▪  Narrow           Ctrl+Shift+N
  ▬  Full width       Ctrl+Shift+G
  ─────────────────────────────────
  Focus mode          (   ) Coming soon
```

It stays open while you try things — every row there is a setting whose result
you are looking at — so `Esc`, or a click outside, is what closes it. It also
stays where you opened it, even though switching the width moves the page
underneath.

**Page and pageless** — the three widths:

| | |
| --- | --- |
| **Page** | the document on a centred page (820px) over a grey canvas, the way Google Docs and Word show one |
| **Narrow** | no page, but the lines wrap at the same 820px measure, centred |
| **Full** | no page and no limit — lines fill the window. This is the default |

Click a row, press its chord, or press `F7` to cycle them. **Color** sets the
color of the **canvas** — the area around the page — not the page itself. It
sits under **Page** because it belongs to it, and is greyed out in the other two
where there is no canvas to tint. Click a circle to pick one, or the row itself
to step to the next; the first is **Auto** (it follows your light/dark theme,
and the rest are tuned to match it). The mode and page color are remembered per
document.

**Focus mode** is listed but not built yet, which is why it is greyed out.

The **default** mode for new documents is a global preference — set it on the
**welcome page** (the *Preferences* section). A document's own choice, made in
this menu, overrides it. **Apply to all** on the same page switches every open
document to the chosen layout and clears per-document overrides, so the rest
follow it too.

The footer says only what applies here: the three document typefaces, undo, and
whether you are sharing. Theme is chosen on the welcome page and is not listed.

**Prose does not nest; a task list does.** In prose every line starts at the left
margin and `Tab` types a tab rather than indenting the line under the one above
it. Inside a list — and on any task row — `Tab` nests and `Shift+Tab` brings it
back out, bounded by the document so a line can never escape it. `F4` hides
ticked rows wherever they are. Structure comes from **headings** instead, and a heading is a property of the
line rather than a place in a tree — so making one, and unmaking it, never moves
anything.

**…except inside a list.** A list item is the one place a document *does* nest:
`Tab` turns an item into a sub-item (so a numbered list can read `1, 2, 2.1`),
and `Shift+Tab` brings it back out **in place** — the row stays above whatever
was nested under it or listed after it at the same level. With several blocks
selected, both keys move the selected roots together and keep their order;
selected children travel with their parent rather than moving twice.
`Ctrl+Shift+↑` / `↓` reorders the same selection; a fully selected list stays
one block even when paragraphs beside it are selected too. The markers are drawn
for you and never typed — a `3.` is just "the third numbered item in a row",
which is why deleting the middle one renumbers the rest. `Shift+Tab` on any
outermost item removes its marker and leaves a paragraph in the same place;
items after it become a new list whose numbering starts at 1. Apply the same
list style to that paragraph or middle list to reconnect compatible lists on
both sides. `Backspace` at the start of a list row does the same outdent.
`Enter` on an empty item steps out one level the same way (repeated Enter walks
out to ordinary text); `Enter` on a non-empty item continues the list.

Three ways to make a heading, all the same thing underneath:

| | |
| --- | --- |
| the bubble | select some words, then the `Text ⌄` dropdown; the level you are in is ticked |
| a chord | `Ctrl+Alt+1` … `4`, and `Ctrl+Alt+0` for ordinary text |
| typing | `#` at the start of a line, then a space — `##` for a two, and so on |

The hashes disappear when the line becomes a heading: there is no markdown in a
document here, only the heading itself. If that is not what you wanted, `Ctrl+Z`
puts them back as ordinary text.

Five ways to start or re-style a list:

| | |
| --- | --- |
| the selection bubble | select text or blocks, then choose the bulleted-list or numbered-list icon |
| a chord | `Ctrl+Shift+8` for bullets; `Ctrl+Shift+7` for numbering; press the active style again for paragraphs |
| the `+` | in the margin of any line — bulleted, numbered, to-do, lettered or roman |
| typing `/` | the same menu, without reaching for the margin |
| typing | `-`, `*` or `+` then Space for bullets; `1.` then Space for numbering; `[]` then Space for checkboxes |

Typed markers work at the beginning of existing text as well as on an empty
line: placing the caret before `two` and typing `- ` turns that same `two` row
into a bullet. The marker disappears; the text is not replaced.

The `[]` disappears — the row is wrapped into a `ListBlock` container, and the
row becomes its first item. `/tasks` from the slash menu does the same. The menu
wraps when the caret is in prose and re-styles when it is already in a list.
`Tab` nests; `Shift+Tab` and `Enter` on an empty item outdent one level in place.

Typing `/` also offers headings — `/header2`, or just `/h` to see the four.

Three ways to make a quote:

| | |
| --- | --- |
| typing | `> ` at the start of an ordinary line |
| typing `/` | choose `/quote` from the block menu |
| paste | copy a rendered Markdown/HTML blockquote and paste normally |

The `>` is a gesture and disappears; the quote is stored as a semantic block
with an inset and a vertical rule. Enter continues a non-empty quote. Press
Enter on an empty quote to return to ordinary text. Copying a quote preserves
it as a blockquote for rich destinations and writes `> ` in plain text;
`Ctrl+Shift+V` deliberately drops the quote structure.

ChatGPT's browser copy uses semantic `<blockquote>` HTML. Koru reads that HTML,
ignores ChatGPT's changing CSS classes, and keeps consecutive quoted paragraphs
under one continuous rule. If a source supplies only plain text rather than an
HTML clipboard flavor, its `>` characters remain ordinary pasted text.

### Tables

Create a table by typing `/table`, then type GFM pipe-table source and press
`Esc` to validate and show the grid. Ordinary paste does the same when
the clipboard advertises Markdown or contains a high-confidence pipe table:

```markdown
| App | Notes |
| --- | ---: |
| Koru | **Fast** and `local` |
```

The grid renders bold, italic, strikethrough, inline code and links inside
cells, and respects delimiter-row alignment. Click a cell and type to replace
its contents. `Tab` moves across cells, `Shift+Tab` moves back, and Enter moves
down; moving past the last cell adds a row. Right-click a cell for row/column
commands or **Edit Markdown**. `Ctrl+Enter` creates a fresh paragraph directly
below the table and puts the caret at its start.

Rest the pointer on a table to reveal two floating buttons at its top-right.
They use the standard tooltip timing: a 400 ms intentional-hover wait, a 100 ms
fade in and a 75 ms fade out. They overlay the table and reserve no empty row of
their own. The horizontal-arrow button toggles its width policy — outward
arrows offer Extended width and inward arrows offer Compact width — and the
code button opens its Markdown source. While editing source, the table button
validates it and returns to the grid. `Esc` does the same while source is open,
and the context menu shows that shortcut beside **Show table**. Row/column insertion and
deletion stay disabled until the grid is visible. Rendered tables have 4 px of
air above and below, so two adjacent table blocks remain separated by 8 px
without inflating either block's selection area. Plain `Enter` on a focused
rendered table creates a paragraph below it rather than splitting its Markdown.
**Compact** (the default) fits and wraps within the document measure.
**Extended** retains wider, content-driven columns and scrolls inside the table
with a horizontal wheel or `Shift`+wheel when they overflow. This setting syncs
with the document; control visibility and the current scroll position do not.

If the delimiter row or column count is invalid, Koru leaves every source byte
visible and shows a line-specific error. Fix the source and press `Esc`
or the table button again. Plain paste (`Ctrl+Shift+V`) deliberately does not
create a table.

Pressing a heading you are already on returns the line to ordinary text, so one
button is both there and back. A heading is a line like any other: it takes
pills, bold and a highlight, and `Enter` at the end of one starts a plain
paragraph underneath — while `Enter` in the *middle* of one gives you two
headings, which is what breaking a title in half means.

**A list block has a name.** It appears in the explore column (as "List N" until
you rename it), and clicking it there scrolls to it in the document. You can
also open a list as its own tab — the same way you open a document.

**A horizontal rule** breaks a document into sections. Two ways to drop one in,
both leaving the caret in a fresh empty line beneath it:

| | |
| --- | --- |
| typing | `---` on an otherwise-empty line |
| the menu | `/divider` (or `/d`) |

`--` mid-sentence is left alone — it is an em-dash waiting for its pair, not a
rule. A divider is a block like a picture: drag the handle to move it, `Enter`
to open a paragraph below it (`Shift+Enter` for one above), `Backspace` to
remove it. It carries no text. Copying one out of a document writes `---`, the
way a heading writes `## `.

**Click the empty space below the last line** and the caret lands at the end of
the document, ready to type — adding a line if the last one is already written
on, and reusing it if it is blank. The page behaves like a page.

Two tasks can reference the same document: type the same name and you get the
same document, not a second one with a matching title.

## Importing content

Import one or several Markdown files (`.md` or `.markdown`), TextBundle
packages (`.textbundle`), compressed TextPacks (`.textpack` or `.zip`),
Microsoft OneNote content (`.one`, `.onetoc2`, `.onepkg`, or a ZIP containing
OneNote files), a Notion Markdown & CSV export (`.zip`), local Evernote exports
(`.enex`), or Google Docs into the
workspace you have open. The format choices are available in three places:

- click the `+` beside the tabs, then **Import…** and choose a format
  (Markdown, TextBundle, Microsoft OneNote, Evernote, Google Docs, or Notion);
- click the workspace name, then **Import content…**, and choose a format
  (or **Import…** under the open profile's workspaces in the **Manage** tab,
  which opens the same chooser);
- on the Welcome tab, under *Bring your documents in*, press the format's button.

You can also drag one or several supported files or packages from File Explorer
or Finder anywhere into the Koru window. A `.zip` is sniffed: OneNote packages
first, then Notion exports (filenames ending in a 32-character id), otherwise
TextPack. Imported notes become ordinary Koru documents in the current
workspace. For Markdown, the first H1 is used as the page title and is not
repeated in the body; without an H1, the filename is the title. Headings,
paragraphs, quotes, lists, tasks, code blocks, tables, dividers, links and
common inline formatting are converted to their Koru equivalents. Leading YAML
frontmatter is preserved as a YAML code block.

Import begins in the background. Its toast stays visible until the work ends or
you close it; closing the toast does not cancel the import. A measured operation
shows its current percentage from 0–100, while work whose total cannot be known
yet shows a continuously moving progress bar. The toast is replaced by the
success notice when the import finishes, even when a large OneNote package
takes a minute or more.

Relative image links are resolved beside the Markdown file and their image data
is copied into Koru. Remote or missing images remain as links so the reference
is not lost. A batch is best-effort: a file that cannot be read does not stop
the rest. Koru opens up to the first ten imported notes as tabs, selects the
first one, and leaves any additional notes in Explore. The completion toast
offers **View**.

A TextBundle contains one Markdown note, metadata and an `assets` folder. Koru
accepts TextBundle versions 1 and 2 when their content type is Markdown. It also
accepts the same package compressed as a TextPack, including archives with one
outer wrapper folder. The contained note follows the same Markdown conversion
rules, and its local assets are copied into Koru.

OneNote import is offline: Koru reads the export files directly and never asks
for a Microsoft account. Notebook, section-group, section, page, and subpage
hierarchy becomes nested pages. Rich text, lists, tables, images, and embedded
files are converted. Handwriting is represented by a visible placeholder, and
equation source is retained with a warning where the export parser cannot
produce a rendered equation.

Links between OneNote sections, pages, paragraphs, and other addressable blocks
in the same import become native Koru document links. Following a block link
opens its page, scrolls the block into view, and briefly highlights it. If the
target was not included in the import, or an exported identifier is ambiguous,
Koru keeps the original `onenote:` link clickable and lists it in the imported
**FAILED** page. On a computer with OneNote installed, the operating system may
still be able to open that original link.

Google Docs import goes through Google, not through files: choosing
**Import… → Google Docs** opens your browser to Google's sign-in, which doubles
as the document picker — you select one or several documents and confirm.
Koru can see only the documents you pick — never your whole Drive — and each
one is exported as Markdown and imported with its headings, lists, tables,
links and embedded pictures, the same way a Markdown file would be. The
sign-in's refresh credential is stored in your computer's secure credential
store (Windows Credential Manager, macOS Keychain, or the Linux Secret
Service), not in a Koru file; short-lived access is kept only in memory. If
Koru is running without its Agent, import still works but the sign-in lasts for
that process only. Closing the picker without choosing simply cancels the import.
A build of Koru made from source without a Google sign-in registration shows
**Google Docs** dimmed, with *Not set up in this build* beside it;
[CONTRIBUTING.md](../../CONTRIBUTING.md) explains how to supply one.

Notion import is offline: export from Notion as **Markdown & CSV**, then choose
**Import… → Notion** (or drop the zip). Koru strips the long ids Notion appends
to filenames, restores the page hierarchy under one container named after the
export, turns relative page links into Koru document links, copies images and
other files in, and converts full-page databases into tables with the CSV kept
as an attached file.

Evernote import is offline: export one or more notebooks from Evernote as
`.enex`, then choose **Import… → Evernote** or drop the files into Koru. Each
source file becomes a container named after the file, with its notes nested
inside. A repeated import always creates a new container; if the name is already
used, Koru adds `(2)`, `(3)`, and so on. It does not silently merge with or
replace an earlier import.

V1 keeps headings, paragraphs, lists, quotes, code, common formatting, links,
simple tables, checklists, tags, images, PDFs and other attachments. Attachments
also appear in Files. Exact Evernote GUID links between selected notes become
Koru links; an ambiguous link stays an `evernote://` link and is reported.
Encrypted content is not decrypted: the rest of the note imports with an
**Imported encrypted content** placeholder. Modern Evernote Tasks keep their
visible text but do not become Koru tasks. Their completion status, due date,
reminder, recurrence, flag and assignee are not imported; **FAILED** names each
affected note. Ordinary Evernote reminders do not become due dates, and merged
table cells are flattened while their text is kept.

A note Evernote marked deleted or inactive is created directly in the workspace
Trash, using Evernote's deletion time when it is valid. It never flashes in the
live library first. If the running Store is too old to create it that way, Koru
stops the selected job before writing anything and asks for an update/restart.

One malformed note is omitted without stopping its siblings. An ordinarily
truncated ENEX keeps notes that were already complete. A security rejection
(for example an internal entity declaration or an XML limit) discards that
whole source—documents, resource puts and tag changes—while another selected
file can still import. Malformed vendor application data attached to a resource
is ignored because it contributes neither note content nor attachment metadata;
the note and attachment still import. Warnings are collected in one **FAILED**
page per affected source. Evernote's normal external
DOCTYPE declarations are accepted but inert; Koru never fetches them or any
other network resource during this import.

### Attachments and files in a page

Type `/attach` (or use the `+` menu's Attachment row) to pick one or several
files. Each becomes an attachment pill on the page and a row under Explore's
**Files** mode. You can also type `@` and search for a file already in the
workspace — or restrict the search with `file:` (files only) or `doc:` (pages
only). Clicking an attachment pill opens it in the app the operating system
associates with that type. Right-click it for **Open in app**, **Reveal in
File Explorer** (switches Explore to Files and scrolls to the row), **Save
as…**, or **Remove** (deletes the pill from the page; the file stays in
Files). If a file is in the Trash, the pill shows the same trashed treatment
as a binned document; if the file is gone entirely, the pill draws a broken
mark and a strikethrough name.

The two icon buttons at the top of Explore switch between **Documents** and
**Files**. Embedded OneNote or Notion files appear in Files and as attachment
pills in their pages. File rows use the file's MIME type (and its extension for older
entries) to distinguish PDFs, Office documents, Markdown, media, archives and
source files at a glance. Files are listed newest first. Clicking selects a
file; it does not open or save it. **Double-click** opens it in the app the
operating system associates with that type — a shared web link opens in the
browser. `Ctrl+click` toggles individual files, `Shift+click` selects a range,
and a drag moves the whole selection just as it does in Documents. Right-click a
selection and choose **Open in app** to use the operating system's associated
application, **Rename…** (`F2`) to change the name, or **Save as…** to write
another copy. With several files selected, **Open in
app** opens every selected file and **Save as…** asks for one destination
folder, then writes every selected file into it. With one file selected,
**Save as…** keeps the ordinary file-name picker. **Add files** in an empty Files view
and the import icon in the Files heading accept one or several arbitrary files
through the same batch path as drag-and-drop. File bytes are copied into the
workspace, so moving or deleting the original does not break the attachment.
If the workspace already has a live file with the same BLAKE3 content hash,
Koru keeps the existing row and reports that the file is already known instead
of adding a duplicate. In a page, type `/attach` to pick a file and insert an
attachment link at the current row.

**A file can carry one note** — an ordinary Koru document attached to it. The
note icon to the right of a file's name opens the note as a tab; the tab wears
the file's icon and the file's name, and renaming the file retitles the note.
The right-click menu offers **Add note** on a file that has none and **Open
note** on one that does, for a single file only — a multi-selection gets
neither. A note never appears as a row of its own in Documents or Files, but
it is indexed like any other page and turns up in search. Trashing the file
trashes the note with it.

**Files added on another device appear here before their contents do.** A file's
name, type and place in the workspace travel with everything else, while its
contents wait until somebody wants them — otherwise every device would download
every attachment anyone ever added. A file whose contents are not on this device
is drawn greyed with a download arrow instead of its usual file icon; **click it
once** and Koru fetches it from whichever of your linked devices has it. It then
behaves like any other file and stays available offline, including after a
restart. Files already on this device are unchanged: a plain click selects them,
double-click opens them, and Open in app, Rename… and Save as… stay on the
right-click menu.

If no linked device is reachable the download waits rather than failing, and
finishes when one comes back. Files above 32 MB cannot be transferred between
devices yet and say so.

A content import keeps a progress toast visible until it finishes. **Stop** at
the end of the progress bar requests cancellation at the importer's next safe
checkpoint; the toast reads **Canceling** for three seconds and then closes.
Closing the toast with **×** only hides progress and does not cancel the import.
For Evernote, an already-running Store write is allowed to finish; the current
uncommitted source and its temporary resource spools are then dropped. Notes
whose Store commits already completed remain valid.

### Sharing into Koru from another Mac app

On macOS, Koru appears in the **share sheet** — the Share button in Safari,
Preview, Photos, Finder's right-click menu, and anywhere else the system offers
one. Share a page, a link, some selected text, one image or several files, and
Koru comes to the front and adds them to the workspace you have open, the same
way dragging the files in would.

Two things worth knowing:

- **Sharing works whether or not Koru is already running.** What you shared is
  written to a staging folder first; Koru picks it up the moment it is in front
  of you. If it is not running, sharing starts it.
- **Nothing is lost if the import cannot happen yet** — no workspace open, or a
  transfer that failed. What you shared stays in the staging folder and is
  picked up the next time you come back to Koru.

If you install more than one Koru — a stable build and a beta, say — each one
gets its own row, labelled with its channel, so you can tell which will receive
what you share. Sharing into Koru is not available on Windows.

**And this row has not been seen yet.** It appears only in an installed,
signed build — a development copy never registers with the share sheet —
and nobody has installed one and shared to it end to end. Everything on
either side of the row is built and checked; the row itself is the
unproven step.

## Moving a document to Trash

Deleting a document does not remove it. It goes to the **Trash**, and stays
there until you empty it — nothing clears it on a schedule, so a page you binned
last year is still there.

**Select first, then move.** Click selects one document. `Ctrl+click` toggles
individual documents; `Shift+click` selects the full tree range from the last
clicked document, including the visible children of both ends. Open the `…` or
right-click anywhere inside that selection. The menu offers **Move to…** first
(`Ctrl+M`), then **Export → Markdown** or **TextBundle**, then **Move to
trash** / **Move N nodes to trash** — multi-select moves, exports, or bins the
whole set.

**Move to…** opens a centred picker of every workspace in this profile. Each
workspace row shows the same colour and emoji tile as the switcher. Walk with
`↑` `↓`, expand with `→` (or the caret), collapse with `←`. **Enter** on a
workspace puts the document at the top of that library; **Enter** on a document
makes it the last sibling; **Shift+Enter** makes it a child. **Alt+Enter** does
the same and follows the document (switches workspace when needed). Typing
filters by title across **all** workspaces in the profile. An unopened
workspace's listing is fetched only when you expand it; opening the picker does
not preload every workspace. A cross-workspace transfer continues on a Store
worker so a large document or file set does not freeze the window. A toast
offers **Undo**. Moving to another workspace in the same profile is supported;
another profile is not. If nothing is selected to move, the toast says so.

Moving selected rows that contain images or files copies their bytes into the
destination workspace before the source is replaced with its trail link. Those
images and files remain available after switching workspaces or restarting
Koru. A reference made by an older build without that copy is repaired from
another local workspace in the same profile when the bytes are first requested.

**Export → Markdown** writes one document via a save dialog prefilled with the
title, or several into a folder picker (one `.md` each). The last folder you
exported to is remembered. When the export finishes, a toast offers **Show** —
it opens the file (or the folder, for a multi-export) in File Explorer / Finder.

A link to another Koru page exports as `[[koru://doc/…]]`, carrying its heading
when it has one, and importing that Markdown back into Koru turns it into a
page link again. A link Koru does not recognise stays as ordinary text rather
than failing the file.

**Export → TextBundle** writes a TextBundle v2 package containing
`info.json`, `text.markdown` and any local images under `assets/`. A single
document defaults to `.textbundle`; use a `.textpack` or `.zip` filename to
write the same package as a ZIP archive. A multi-document export writes one
`.textbundle` package per selected document into the chosen folder.

You can also drag any selected row to move the whole selection; you do not need
to keep `Ctrl` held for the drag itself. When two or more documents will move,
they collapse to one carried row showing the total, including documents inside
a selected parent. Selected children collapse into their selected parent first,
because the parent carries its whole subtree. Drop in the middle of another
document to make the selected roots its children, or below the tree to put them
at the Library root. The moved roots remain selected after the drop.

Drag the selection onto **Trash** at the foot of the sidebar to bin it. The row
lights up and says **Move N items to trash** while you hover there. The menu and
the drop use the same Trash path, including asking when there is a lot inside.
Moving several selected roots asks about each of them in turn, and saying no to
one cancels the rest. `Delete` and `Ctrl+Shift+Delete` are deliberately not
shortcuts for this action.

**A page with pages inside it takes them with it.** If there are more than three
of them you are asked first, and told how many:

> **Move 24 items to Trash?**
> "Q3 Launch" contains 23 nested items. They will be moved together and can be
> restored from Trash.

Child pages are organization in the library, not blocks in the parent page.
Their titles and contents appear only when you open those child pages; they are
not repeated at the end of the parent document.

Note the count is **pages**, not lines: a long document with no sub-pages goes
straight to the bin without stopping to ask.

**Enter cancels.** The red button is reached with `Tab` or with the pointer, on
purpose — a dialog that appears under your hands should not read the next key you
happen to press as "yes, delete all of that".

A message appears at the bottom afterwards saying what went, with **Undo** beside
it for a few seconds. Undo puts it straight back where it was.

## The Trash

At the foot of the sidebar there is a **Trash** row. It always opens, including
when the bin is empty. Click it to open the Trash, which lists what is in there,
where each thing came from, and when it went.

The table shows where each thing came from, when it went, and who put it there —
**You** when it was this window, and the other person's name when it was not.
(If nobody has set a name it says *Someone else*; Koru will not guess.)

Click selects one row in Trash. `Ctrl+click` toggles individual rows, and
`Shift+click` selects the full range from the last click. Hover a selected row
for **Restore**, or use its `…` for *Restore* and *Delete permanently*; either
action applies to the complete selection. An unselected row becomes the sole
selection before its controls act. The `…` beside the heading has one option:
**Empty Trash**. Permanent deletion asks once for the selected set and cannot be
undone. The `Delete` key is not a permanent-delete shortcut.

When permanently deleting selected items, Koru processes each item independently.
If some deletions fail, the error says whether other items were deleted;
successful deletions remain in effect. The message follows the interface language.

Deleted delegated runs appear in this same list. **Empty Trash** includes them
in the confirmation count and permanently deletes them through the same
confirmed action as the other rows.

Restore puts a document back where it came from. If it is a child of a document
that remains in Trash, Koru moves the restored child to the workspace root; it
does not restore the parent or report success while leaving the child hidden.
**Save a copy** follows the same rule, so the new copy is live rather than being
created beneath the trashed parent. If the original place no longer exists, the
document likewise returns at the top of the library.

> **What "Delete permanently" means.** Trashing a document does not make the file
> on disk any smaller: the page is still inside your workspace, just somewhere
> nothing shows it. Emptying the Trash is what actually removes it.

## Document history: comparing versions

Every document quietly keeps versions. One is captured after a couple of idle
minutes, another during long stretches of continuous editing, and **Save
version** pins one on demand — before a big rewrite, say.

`Ctrl+S` does the same with a name: a small prompt asks what to call the
snapshot, and the named version shows its name in the list in place of
*Saved*. If nothing has changed since the newest version, the same prompt
offers to **rename** that snapshot instead — saving an identical copy twice
would only clutter the list. Leaving the name empty saves unnamed (or, in a
rename, clears the name back to its reason).

Open the right-hand context panel's **History** section to see them, newest
first, each with why it exists (*Saved*, *While editing*, *After editing*, …)
and when. Click one version and the button reads **Compare with current** —
what has changed since. Click two and it reads **Compare selected** — the
difference between them. Either way the comparison opens as its own tab:

- An added block sits on a green tint; a removed one on red, struck through,
  at the place it used to be.
- An edited block shows its words: what left is struck in red, what arrived
  is highlighted in green.
- A block that merely moved or changed order says so in one line, with the
  start of its text, rather than repeating itself in full.
- Long unchanged stretches fold into "*n* unchanged blocks", keeping a few
  blocks of context around every change.

The diff always reads **older → newer**, whichever order you clicked, and the
header says which versions it spans. The comparison keeps the document's own
page layout — Page, Narrow or Full — so it reads at the same measure as the
page it is about. Comparing is only a question: it never
saves a version and never touches the document, and the tab is a working view
— it is not reopened after a restart. Restoring an old version is not in this
panel yet; it exists today through Koru's MCP tools.

## When somebody else deletes a page you have open

If you are sharing a workspace and somebody bins a document you have open, **your
tab stays open** and grows a strip across the foot of the page — at the bottom
rather than the top, so it does not shove the first line of your writing down the
screen every time a peer bins something:

> **Sam moved this document to Trash**
> It left the Library while you were editing. Your unsynced changes are still
> here — restore it to keep editing, or save your version as a separate
> document.
>    [ Restore ]  Save a copy  ×

You can carry on typing — nothing you write is lost either way. There are two
answers:

- **Restore** puts the document back where it was, and the other person is told
  that you did.
- **Save a copy** makes it *yours*: a new document, named `… (copy)`, that opens
  straight away. The original stays in the Trash, so nobody has to argue about
  whose decision wins.

The tab of a document that is in the Trash wears a small bin icon, so you can see
it without switching to that tab. The × dismisses the message for that document
only — another one being deleted will still tell you.

If nobody has set a name on their profile it just says *This document was moved
to Trash*, with no name attached. Koru will not guess: it would rather say
nothing than tell you the wrong person deleted your page.

## More than one window

A tab is a thing you opened; a **window is a whole workspace**. Most of the time
a tab is what you want — `Ctrl+N` is for when you want two of them on screen at
once.

`Ctrl+N` opens another window on the same workspace. Both show the same work,
each keeps its own tabs, caret and scroll position, and typing in either shows up
in the other straight away — there is nothing to save or refresh.

Useful for keeping a reference list visible while you work further down, or for
having a document open beside the one that links it. Only the first window's
tabs and shape are the ones that come back next launch.

**A popped-out document is a window too.** Closing one asks what you want: close
it outright, or **restore it as a tab** back in the main window. Tick *remember
my choice* and Koru stops asking — it reuses your answer for every popped-out
window from then on (change it by editing `popout_close` in the settings file:
`ask`, `close`, or `restore`).

Closing a window closes that view and nothing else, whichever window it is —
close one and a document opened from it stays open, and Koru quits when the last
window goes. The size and position that come back next launch are the
first window's; once that one is closed they stop moving, rather than being
replaced by the shape of whatever document you happened to close last.

To open at a specific size instead of the remembered one:

```sh
koru.bat --window-size 1024x768       # ./koru.sh --window-size 1024x768
koru.bat run -- --window-size 1024x768
```

The size is the inner client area in logical pixels, at least 720x600. A
maximised last session is not restored.

## Working with someone else

Launch two instances, each with its own name:

```sh
koru.bat run -- --peer A       # ./koru.sh run -- --peer A
koru.bat run -- --peer B
```

They share one workspace — the same folder, which is what makes them the same
document. (`--peer` gives each its own name, its own window position and its own
update file; it does *not* give them separate content. Separate content is a
separate profile — `--profile <path>`.) Type in one and it shows up in the other within about a third of
a second. The status bar's right-hand slot tells you which peer you are and
whether you are sharing.

`F5` stops and starts sharing. This is worth trying deliberately:

1. Press `F5` in both windows so each reads `offline`.
2. Edit the *same task* in both — type at the start of the line in one and the
   end in the other, bold a word in one, indent it in the other.
3. Press `F5` in both again.

Everything survives. Both edits are in the line, both marks are applied, and the
task keeps the edit one window made while the other was moving it. There is no
"last save wins" and no conflict to resolve.

Close one window, keep working in the other, reopen it — it catches up. Nothing
needs to be running for your work to be safe.

After two people have edited the same line, `Ctrl+Z` takes back your own typing
and leaves theirs alone. The one rough edge left: if the other person deleted
the thing you were editing, `Ctrl+Z` does nothing at all — it declines rather
than bringing back something they removed.

## Installing Koru

**A release is one zip file, and there is nothing to install.** Download it,
extract it wherever you like — a folder in your documents, a second drive, a
memory stick — and run **`koru.bat`**, or `koru.exe` directly. No installer, no
administrator, no registry.

Inside the zip are seven programs and everything they need: `koru.exe`, which is
the workspace window; `koru-ai-chat.exe`, the standalone profile-free AI Chat;
`koru-agent.exe`, which owns the notification-area icon, global shortcuts,
networking, and updating — and also the public API and the Store worker that
writes your content, which it runs as workers of itself;
`koru-com-notification.exe`, the one-shot helper that catches a button press on
a reminder; `koru-mcp.exe`, which an AI host starts on demand;
`koru-voice-worker.exe`, the only program that ever opens your microphone; and
`koru-updater.exe`, which does the replacing when an
update is installed. Beside them are the fonts Koru draws with, the public MCP
contract, a `VERSION` file, a `README.txt` and `release-files.json`, which is
the list a download is checked against.

**You only ever start `koru.exe`.** It brings up the agent, which serves the
public API and its Store worker from further copies of itself, and it looks for
its neighbours **in its own folder** rather than anywhere else on your machine —
so a nightly copy can never end up running a stable one's service chain. If you
open Task Manager you will therefore see several `koru-agent.exe`: that is one
agent and its workers, not several agents. The seven go together: they are
built, signed and released as one thing.

**Your content is not in that folder.** It lives under
`%LOCALAPPDATA%\Koru\<channel>\`, with `store\` (your workspaces, documents and
images) and `agent\` beside it. So the extracted folder can be moved, replaced
or deleted without touching a word you have written — which is also why an
update can replace all seven programs and leave your work where it is. If the
standalone Koru AI Chat is open, the update closes it normally and reopens it
afterward; its secure credential and independent window geometry remain in
their ordinary stores rather than in the installation folder.

## Updates

**The agent does the updating, not the window.** It checks, it downloads, it
verifies, and it drives the swap; a Koru window's part is to show you what it
found and to pass back what you pressed. That is why updating still works when
the window you were shown it in has since been closed.

The window's **Updates** modal is hidden from the overflow menu for now; the
tray's **Check for updates** checks straight away. The modal says what this
build is:

- **Channel:** *Stable*, *Beta*, *Nightly* or *Internal* — a fact about the build
  you are running, not a control. Nightly adds *"Nightly builds may be unstable
  and update frequently."*
- **Current:** the version, the build number, when it was **released** and the
  commit it came from. A build somebody compiled themselves was never released:
  it says `built` and when the executable was produced, and `debug` or `release`
  where the commit would be. Either way the time is in your own time zone.
- **Automatic checks: On / Off** and **Check for updates** — both greyed when no
  agent is connected, because there is then nothing to do the checking.

Escape closes it, and so does clicking anywhere outside the panel.

A build somebody compiled themselves also says which branch it came from, beside
the name at the top: **Koru (dev)**. Two windows from two working trees are
otherwise identical on screen.

### You are asked twice

A download is something you can ignore; an install closes your windows. Agreeing
to the first has never meant agreeing to the second, so Koru asks separately, and
**the release notes come first both times**.

**When a build is found**, a banner appears at the foot of the window: *"Koru
0.4.0 · build 412 is available"* — the same name the tray's About row gives the
build — with the release notes' title and a short summary under it.

| | |
| --- | --- |
| **Download update** | downloads it, and does nothing else yet |
| **Later** | not this build. It is not offered again until a newer one arrives |
| **View all changes** | opens the full release notes in your browser |

While it downloads, the banner reads *"Downloading Koru"* with the percentage and
the megabytes so far, and a **Cancel** button — which stops the transfer without
declining the build, so it is still offered afterwards. Then *"Verifying
update"* — *"Checking the package signature and archive."*

**When it is ready**, the banner reads *"Koru 0.4.0 · build 412 is ready"* and,
under it, *"Koru will close and restart to install this."* The primary button
now says **Restart and install** — a different label, because it is a different
consent, over a build that is now on your disk and checked. You cannot skip a
step by any route; the agent refuses a request that arrives out of order rather
than trusting the banner it drew.

**Your click's fate is always on screen.** While a consent is on its way the
banner says *"Asking Koru's agent…"*; if the agent has not answered after ten
seconds the buttons come back beside *"Koru's agent did not answer. Try again,
or restart Koru."* — and a click that could not be sent at all says immediately
that the agent is not connected. The banner never silently re-arms.

Once the agent hears the consent, everything is quick: your windows close,
and the updater's own small progress window ("Updating Koru") stays on screen
through the swap — it is the one thing running while Koru's files are being
replaced — until Koru reopens by itself.

If Koru cannot replace its own installation — a read-only folder, or somewhere
your account may not write — the first button is **Download manually** instead
and opens the download page. If a step fails, the banner says so and offers
**Try again**.

### What installing actually does

The agent asks **every Koru window** to close. That is the ordinary close, so a
window with unsaved work asks you about it in the usual way — and **saying no
stops the update**. Nothing is replaced, nothing is lost, and the banner comes
back for when you are ready. Windows have half a minute to answer; a window that
never does aborts the update rather than being closed underneath you.

Then the gateway/Store worker is stopped, the updater replaces the six
programs, and the agent starts the new build and **reopens the windows it
closed**. If the new build does not come up healthy, the previous one is
restored and restarted.

**Installing needs about 620 MB free.** The updater stages the package, backs up
everything it is about to replace, and verifies before it moves anything, so it
wants room for several copies at once — package × 5, plus what is installed, plus
a margin. It checks first and refuses with a plain message rather than running
out of room half way through. Your documents are not in the folder being
replaced, so none of that space is your content.

### Channels, and changing one

**Stable**, **Beta** and **Nightly** are three separate builds of Koru.
**A build follows its own channel and never switches itself**, in either
direction — a nightly install stays on Nightly, a stable install stays on Stable,
and neither will silently downgrade you.

**To change channel, download that channel's build and extract it.**
*Download another channel* in the Updates modal opens the download page. There is
no channel switch inside Koru, because flipping one could only ever change which
feed was polled: everything else — where your content lives, which store and
agent this build talks to — is decided by the build itself. As the footnote under
the buttons puts it: *"Koru updates within its own channel. To switch, download
that build and extract it — every channel can be installed side by side."*

**Side by side is real.** Two channels share nothing on disk: stable's content is
under `%LOCALAPPDATA%\Koru\stable\` and nightly's under `…\nightly\`, and each
has its own store and its own agent. Hover the notification-area icon to see
which is which — the tooltip names the version and the channel.

If the channel you are on currently points at a build older than the one you are
running, Koru says so once: *"This Koru build is newer than the selected
channel"*. **OK**, `Esc` or `Enter` puts it away.

**One limitation, and it is a limitation rather than a design.** `Ctrl+Shift+K`
is registered with Windows machine-wide, so **only one Koru can hold it**. With
two channels running, whichever agent started first has it; the second falls back
to `Ctrl+Alt+Space`, then `Ctrl+Shift+O`. **Right-click each
agent's icon to see which chord it settled on** — the *Search* row names it, so
you do not have to work it out by pressing things. If a build could not get any
of them, that row says *Search (no global shortcut)* and `Ctrl+P` inside its
window is the way in. Registration happens once, when an agent
starts: quitting the one that holds `Ctrl+Shift+K` does not hand it over, so
restart the agent you want to have it. Everything else about both installations
works normally — their tray icons, their windows, their own content.

### Opening a Koru link from outside Koru

A `koru://` link opens a page — from a browser, a chat message, a note in
another app. Koru does not claim the link type by itself: right-click the agent
in the notification area and choose **Links → Handle koru:// links**. The row
above it always says who has it now — this build, another build by name, or
nobody — because every channel installs side by side and only one of them can
answer. Switching it on in one build takes it from whichever had it, and the
other's menu will still say otherwise until you reopen it.

**Stop handling koru:// links** appears only when *this* build is the one
registered; removing another program's registration is not Koru's to do. No
elevation is needed and nothing is claimed unless you ask.

## Profiles and workspaces

When a Google account has never been connected to Koru, sign-in asks whether to
create a separate Koru account. **Create new Koru account** confirms; **Cancel** ends
the pending creation. **Continue offline** keeps local content available. If you
leave the desktop choice this way, **Return to account choice** reopens it.
If you meant an existing account, cancel and sign in to it, then on desktop open the
avatar, **Manage profiles…**, and on the Profile's page choose **Add another
Google Account…**.
Returning accounts do not show the new-account choice. A matching verified email
can instead require a fresh sign-in to the existing account before connecting.

If the connection fails after a decision, Retry recovers that same decision.
An expired or interrupted sign-in may have already created the account; sign
in again to discover it. Quitting before sign-in is saved requires a new sign-in.
Local Profiles, workspaces and paired-device trust remain available throughout.

If a saved account connection is missing or no longer usable, **Sign in again**
reconnects it while your local content stays available. A temporary storage or
connection failure may first require Retry so Koru can finish checking an older
saved sign-in. A problem with another saved account does not interrupt an
account that has restored successfully.

A **profile** is you — personal, work, or school. A **workspace** is a place to
put things, and a profile has as many as you like. Both are held by the Koru
Store, not by a folder you chose — see
[Where your work is kept](#where-your-work-is-kept).

**The top of the pane is two buttons onto one switcher.** Your avatar answers
for *who*; the workspace name beside it answers for *where*. The switcher has
two pages and opens over the library rather than pushing it down: the avatar
opens it on **Profiles**, the name on the open profile's **Workspaces**. Choose
a profile and its workspaces slide in from the right, under a back row that
names the profile; the back row (or `←`) slides out again. Choosing a profile
never switches by itself — you switch profile by choosing one of its
workspaces, which opens that workspace in this window. Pressing the other half
of the header slides to the other page; pressing the half whose page is showing,
`Esc`, or a click anywhere else closes it. `→` or `Enter` on a profile goes in.

Click the **avatar** — or press **`Ctrl+Alt+P`** — to open the Profiles page.
It is the same profile choice global search shows at its bottom left. The
shortcut opens whichever profile control is in front of you and leaves Explore
open or closed as you set it.

Under the profiles sit **New profile**, then **Manage profiles…** and **Link a
device…**. *New profile* opens the **Manage** tab on its create form, and *Manage
profiles…* opens it on the open profile's page. The account chooser —
**Accounts…** on Manage's **Koru Cloud** page — switches between held accounts; **Add Account** starts
sign-in or returns to an unfinished account choice. Switching keeps your open
Workspace and tabs, local content, and established device sync. It shows the
selected account's retained Profiles together with local-only Profiles and
reports how many other Profiles are hidden. With no account selected, retained
Profiles remain visible with their account attribution and work offline.
Android's Account screen follows the same selection and local inventory rules.
Signing out removes that account's credential; it does not sign out the others.

The tab lists visible local Profiles and, for the selected Profile, each connected
Google Account. When the device has more Profiles than fit, scroll inside the
left pane; its scrollbar appears only while the list overflows. Profiles remain
selectable and eligible empty Profiles remain removable when Koru cannot reach
the public API. **Unlink** removes only that Google Account's authorization for
the selected Profile; it stays connected to other Profiles and never deletes
local content. Removing the last Google Account asks whether to make the Profile
offline-only, then requires a fresh sign-in with that exact Google Account.
Koru changes the row only after the public API acknowledges it. The Profile,
its workspaces, established trusted-device sync, and offline access remain.
**Add Google Account** opens the system browser for a fresh Google sign-in.
Returning from Google does not connect anything yet: Koru shows the verified
email and asks whether to connect that exact account to the selected Profile.
**Connect Google Account** changes the row only after the account service
acknowledges the exact Profile authorization. **Cancel** abandons that candidate
on the service and changes no account, Profile, trusted device, workspace, or
content. If Connect and Cancel cross in flight, Koru reports the result the
service already committed rather than guessing locally.

If hosted account coordination says an old account was erased, the selected
local Profile remains usable offline. **Reconnect with Google** shows the
warning first, requires fresh Google authentication, and changes the local
`koru:` binding only after the server acknowledges the exact Profile. A live
identity mismatch is never overwritten and an email match never reconnects a
Profile.
**Remove Profile from this device** removes an empty Profile and its settings.
When it still owns Workspaces, **Remove Profile and All Workspaces...** asks once,
moves those Workspaces to recoverable Trash, and then removes the Profile. It
does not change other devices or erase hosted account metadata. The hosted
recovery listing can therefore appear again after sign-in. Removing the last
local Profile returns to onboarding.

A connected Profile separately offers **Delete from Koru...**. After a
Cancel-default confirmation and, when needed, a fresh Google sign-in, Koru
first removes the Profile from the hosted identity and then performs the same
recoverable local cascade. An unavailable service changes nothing and offers
retry or the distinct device-local removal. A device that learns later that a
local Profile was deleted elsewhere blocks that Profile behind three choices:
**Add it again** restores the same Profile to Koru from this device without
changing Workspace Trash; **Remove from this device** moves its active
Workspaces to Trash; and **Continue offline** retains the local Profile and
content but detaches it from the Koru identity so it cannot sync. Add it again
reactivates only this device. Other devices rejoin through normal enrollment.

After Google sign-in, onboarding lists every Profile authorized for that Koru
identity and requires an explicit row selection even when there is only one.
If more authorized Profiles exist than fit, scroll inside that list; the action
buttons remain fixed below it and the scrollbar appears only while needed.
Each row says where its content can come from. A Profile retained on this device
offers **Restore on this device**; its Workspaces remain in Trash until restored
separately. A Profile held by another trusted device offers **Connect this
device** and waits for that device to approve and send content directly. A
Profile with no available copy remains visible and explains that another device
which still has it must sign in. Koru starts new-Profile creation automatically
only when the hosted identity genuinely has no Profiles.

If a Profile has no active Workspace, the Explore column offers **Create
workspace** first. Document creation commands lead to that same Workspace-name
prompt instead of trying to save into the local Welcome placeholder.

Click the **workspace name** — or press **`Ctrl+Alt+W`**, which leaves the
Explore pane open or closed as you set it — and you get:

- every workspace in this profile, with the one you are in shown in bold.
  Clicking one switches to it; **Ctrl+click** opens it in its own window instead;
- a fixed **New workspace** bar, which asks for its name, then makes it and
  switches to it. Cancelling creates nothing;
- the bar's **`…`** menu, containing **Import content…** (with the same import
  formats as Explore) and **Manage workspaces…**, including its shortcut.

The panel is at most 300 px tall. A long workspace list scrolls between the
heading and that fixed command bar, so its creation and management actions do
not move out of reach.

`↑` and `↓` walk the list, `Enter` picks, `Esc` closes it and leaves the
keyboard where it was.

Each row wears a small tile carrying that workspace's colour and emoji, so a
list of six is something you recognise rather than something you read. A
workspace that has neither still gets a tile, in the panel's own grey — the
names all start at the same place either way. Hovering a row also reveals a
pin or unpin button beside its `…` menu, so keeping a workspace at the top is a
single click.

**Right-click a workspace — or hover it and click the `…`** — for everything
else you can do to one:

| | |
| --- | --- |
| **Open**, **Open in New Window** | the same as clicking and Ctrl+clicking it |
| **Rename…** | edits the name in the row itself — `F2` over a row does the same. Enter saves, Esc cancels, and clicking away saves |
| **Change Icon & Color…** | pick an emoji from the same searchable, categorised picker as `/emoji`, including skin tones, and choose one of ten colours. With no emoji set, the tile shows the workspace's first letter |
| **Pin to Top** | keeps it at the top of the switcher |
| **Export Backup…** | writes the workspace, every document and every image to one import-compatible `.koru` file |
| **Archive Workspace** | takes it out of the switcher without deleting anything. If it is the current workspace, Koru opens a neighbour or returns to the welcome screen |
| **Delete Workspace…** | see below |

A name that is empty, or that another workspace already has, is refused **in the
field**: the box turns red, a line under it says why, and what you typed stays
there to fix. Nothing pops up over it.

Archiving or deleting the current workspace opens the next one, then the
previous one if there is no next row. Archiving or deleting the last active
workspace returns to the welcome screen.

**Deleting a workspace does not erase it immediately.** It moves to the
manager's **Deleted** section and can be restored for 30 days, with its archive
state, documents and images intact. If other windows have it open, Koru reports
how many and asks before disconnecting them. The Store alone purges expired
deleted workspaces. A deleted workspace's context menu offers **Restore
Workspace** in place of archive and delete.

### Managing workspaces

Under a profile's workspaces sit **New workspace** — made in the profile you are
looking at, which this window then switches to — **Import content…**, which
offers the import formats, and **Manage workspaces…**.

**`Ctrl+Shift+O`**, or *Manage workspaces…* at the foot of the switcher, opens
the **Manage** tab on a profile's page, at its **Workspaces** — the open
profile's for the shortcut, the profile the switcher was listing for the menu
row. There is one Manage tab: pressing either again brings it forward, and it
comes back after a restart like any other tab (a session saved by an older
version with a separate workspaces tab reopens on Manage too).

The section lists that profile's workspaces, pinned first, then in the
profile's own order, each with its color-and-icon tile and *Opened 3 hours ago*
under its name once it has been opened. The workspace this window has open is
tagged **Open**, and a pinned one **Pinned**. **New workspace** makes one in the
profile whose page you are on; **Import…** — only on the open profile's page —
imports content into the open workspace.

A click selects a row; a double-click, or Enter on it, opens it — switching to
its profile first when it belongs to another one. `F2` renames the selected
row in place. Each row's **•••**, or a right-click anywhere on it, opens the
workspace's menu: Open, Open in New Window, Rename…, Change Icon & Color…, Pin
to Top, **Move Up** and **Move Down** within the profile's order, **Move to
Profile ▸**, Copy ID, Export Backup…, and Archive, Unarchive, Delete or Restore.
A row that cannot be chosen is dimmed and says why when you point at it —
*Open* on the workspace you already have open, or *Open in New Window* on
another profile's workspace (*Switch to this profile first*).

**Show archived (n)** and **Show trash (n)** under the list open them in
place, with **Unarchive** and **Restore** on each row. Whether the archive is
open is remembered for the window; the trash is not. Deleted workspaces are
removed automatically, so the trash has no *Delete forever*. If an archive or
delete only partly finished, a line under the heading says so, with **Retry**
and **Dismiss**.

**Move to Profile ▸** hands the workspace to one of your other profiles.
Choosing one asks first, and names every terminal open in that workspace,
because they close before it moves — a session must not carry on under a
profile you did not start it from. Nothing else changes: the workspace keeps
its id, its documents and its history, and its projects follow it in Koru
Terminal. Moving it back is the same action with the other profile chosen.

If a row says *this window was not started by Koru*, the window was launched
outside the app — a `cargo run`, or a build Koru did not start — and it holds no
permission to archive, delete or move workspaces. If it says its terminal
projects have not caught up, the workspace moved but Koru Terminal was not
answering at the time; choosing the same profile again repairs them.

The **Manage** tab lists every visible Profile, and each Profile's page the
Google, Apple, or email identities explicitly linked to it. Adding or unlinking an identity changes only
that explicit relationship; matching names or email addresses never connect two
Profiles automatically.

On the website, choosing a download starts Google sign-in if needed. A Google
identity new to Koru creates its account automatically. After signing in, you
return to the home page and the selected download starts; there is no additional
account question or success screen. The download buttons remain available if
your browser blocks the automatic download. An interrupted account request offers
Retry sign-in, which continues the same request.

A Profile can be removed directly when it has no active Workspace. If it still
owns Workspaces, **Remove Profile and All Workspaces...** asks once, moves each
one to recoverable Trash, and then removes the Profile locally. Koru keeps the
Profile record, its CRDT, deleted Workspace tombstones, and Workspace databases,
while other devices and the hosted recovery listing are unchanged. Signing in
may therefore list it again even when no current device can supply content. Removing the last
visible Profile does not leave you on a profile-less Manage page. The next launch
enters the same **NeedsProfile** onboarding used by a new installation, where
you can continue offline or sign in before creating a new Profile and Workspace.

**Show archived** and **Show trash** under a profile's workspaces in **Manage**
are independent. Trashed rows never appear in the ordinary workspace switcher;
press **Restore** on one to return it.

A workspace's name, emoji and colour follow the **folder**, not the computer:
they are written into `workspaces/<id>/workspace.conf` as well as into the
profile, so a workspace folder copied onto a stick and opened inside somebody
else's Koru arrives with its name and its colour rather than as an untitled
folder.

Click your **avatar** and you get every profile held by the Koru Store, with the
one you are in shown in bold and a `›` on each row saying it leads to that
profile's workspaces. A profile's emoji and colour are stored with the profile
and are also used by global search. The rows from *New profile* down remain
fixed at the bottom while a long list scrolls inside the panel, which is at most
420 px tall. The picker
does not scan profile folders or show paths from `profiles.conf`; Store profiles are personas, not folders.
Folder-only add, open and remove actions are therefore absent from this list.

Switching a workspace happens in the window you are in, and so does switching
to a workspace in another **profile**: the window changes profile to get there.

The first time you run Koru it makes a profile, an **Example** workspace and a
**Get started** document. That document is yours: change it, or delete it once
you have what you want from it.

**Holding Shift while Koru starts** asks which profile to open, rather than
reopening the last one. `--pick-profile` does the same without the key.
`--profiles` lists them all and exits.

Profiles can be renamed and assigned an emoji and colour from their row menu.
An unset emoji falls back to the profile's initials.

## Where your work is kept

**Koru does not ask you for a folder.** Everything you write — workspaces,
documents, the images you paste — is kept by the **Koru Store**, under
`%LOCALAPPDATA%\Koru\<channel>\store\`. The agent's own settings sit beside it in
`agent\`. *Data locations* in the agent's menu opens either of them, and the
*Store* submenu opens the store's folder directly.

That is deliberately **not** the folder you extracted Koru into: an update
replaces every program in that folder and never touches this one.

To keep a copy of a workspace somewhere you choose — a backup drive, a cloud
folder — use **Export Backup…** on the workspace, which writes the workspace,
every document and every image to one `.koru` file that **Import…** takes back.

To run a completely separate Koru against a folder of your own, start it with
`--root`:

```sh
koru.bat --root "D:\my koru"
```

That is a whole self-contained installation's worth of state — its own store and
its own agent — and it takes nothing machine-wide: no global shortcut, and it
will not update itself.

If Koru opens empty when it should not, check that the Koru Store is running and
that you are in the profile and workspace you expect (the explore pane's
switchers). Content lives in the store, not in a profile folder you once picked.

Everything saves continuously; there is no save command.

## Usage data

**Koru sends no usage data unless you turn it on, on each device.** Koru does
not ask: **Settings → System → Privacy → Share anonymous usage data** is off
until you switch it on. What it shares is which features are used, how well the
app runs, and basic facts about the computer such as its operating system and
screen size, much like what most websites collect when you visit them. It never
includes your documents, their names or anything you type. If you like Koru,
turning it on tells us what matters most to you.

Turning it off takes effect at once and deletes anything not yet sent. Nothing
recorded while it was off is ever sent, even after you turn it on. The choice belongs to this device: another computer of yours has
its own switch. The phone apps send no usage data.

## The tasks in this document

Open the context column with `Ctrl+J` and choose the checklist icon. It lists the
tasks of **the document you are reading** — every row that carries a task marker
or a due date, in the order they appear on the page, with the due date at the
right and completed ones dimmed and struck through. A late one is warm, the same
colour the sidebar and the Tasks tab use.

Clicking a row takes you to it in the document. Nothing here changes a task: the
ways to do that are the row itself under your caret, and the Tasks tab.

It says **No tasks in this document.** when there are none, and it costs nothing
while the column is closed or on another section.

The Tasks **tab** is the other question — every task in the workspace, wherever
it lives — and it is the sidebar's rail that opens it.

## Document history

Open the context column with `Ctrl+J` and choose the clock icon. History follows
the active document. Koru saves versions automatically after two quiet minutes
and every ten minutes while editing continues; **Save version** adds a permanent
manual marker immediately.

`Ctrl+S` asks what to call one first. It opens a small panel with a single name
box: `Enter` saves the snapshot under that name, `Esc` cancels, and leaving the
box empty saves the same unnamed snapshot the **Save version** button takes. A
named snapshot shows its name in the History list where an unnamed one shows its
reason. `Ctrl+S` does nothing on the welcome screen, the Trash or the workspace
manager — there is no document there to snapshot.

**If the document has not changed since its last version, `Ctrl+S` offers to
rename that version instead.** The panel says *Rename snapshot*, the box opens
holding the name it has now — selected, so typing replaces it — and `Enter`
renames it. Emptying the box makes it unnamed again. Nothing is appended: a
second snapshot of state you already have a snapshot of would only make the list
longer. Koru asks the store whether anything has changed rather than guessing, so
the answer is right after a restart and counts an edit made on another device.
When Koru cannot ask — the store is not running — `Ctrl+S` saves, as before.

> Nothing about `Ctrl+S` writes your document: everything already saves
> continuously. A snapshot is a point you can come back to, not a save.

The visible History list refreshes immediately when opened or when the active
document changes, then every five seconds. Selected version IDs and comparison
status survive refreshed rows. Closing History or the context column stops its
refresh deadline and all History listing traffic.

Select two rows and choose **Compare selected**. Koru compares the typed document
states—including moved blocks, task state, metadata and formatting—not an
exported Markdown approximation. The work runs in the background, so opening
History or comparing a large document does not pause typing.

Rows explain why they exist: for example *While editing*, *After editing*,
*Before MCP edit*, or *Before restore*. MCP and restore operations keep paired
before/after points so their effect is inspectable. Restoring a saved version
creates a new change at the head of the document; it does not erase later
history, and other replicas converge on it normally.

History is included in `.koru` exports and whole-Store backups. There is no
automatic deletion in this release.

### If storage goes offline

Koru keeps your documents in the **Koru Store**, a worker inside the API Gateway
that is the only component allowed to write them. If the gateway stops or is
restarting, the word **Offline**
appears at the end of the tab strip, and in a popped-out document window it
appears in that window's own title bar.

**You can keep typing.** Click *Offline* and it tells you where your work is:
either *nothing is waiting to be saved*, or how many documents have changes that
are **saved on this computer** until the store is back. When it returns, they are
sent and the word goes away by itself.

The popover also shows the last connection error and whether Koru is retrying
automatically. A running gateway from another build or channel is not considered
this Koru instance's service; the error text distinguishes that mismatch from a
gateway that is simply absent.

If you close Koru while it says Offline, it asks first — and the question tells
you which of the two situations you are in. Work saved on this computer is safe
and closing costs nothing; if Koru could not save it anywhere, it says so and
names the reason before letting you close.

One thing does not work offline: **inserting an image**. Its bytes go straight to
the store, so `/image` is greyed out with the reason on the row until the store
is back.

### If something has gone wrong

**Your workspace is backed up automatically.** Every time Koru starts it copies
the document aside *before* writing anything, and again once an hour. The twenty
most recent copies are kept in the `backups/` folder inside your workspace. That
first copy of each session is the one that matters: it is your content exactly as
it was before this run touched it.

To take a copy right now, somewhere of your choosing:

```sh
koru --backup
koru --backup "E:\backups\koru"
```

Koru tells you when something is not routine, in a dialog before the window
opens:

- **It recovered from a backup.** The live document would not open. The dialog
  names the backup it used, and where it kept a copy of the unreadable file —
  which is copied, never moved. Check your recent work is there before typing
  more.
- **It is read-only.** The workspace was written by a *newer* version of Koru
  than the one you launched. Nothing will be saved this session; the status bar
  says `read-only` for as long as it lasts. Open it with the newer version.
- **A peer's file was skipped.** Someone else's update log would not read. Your
  own work is unaffected.

**An empty window means an empty workspace** — never a failure to load. Anything
that could have opened blank raises a dialog first, and Koru would rather refuse
to open than let you type all day into something it cannot save.

See [where your work is kept](#where-your-work-is-kept) for the storage layout.
Preserve the files named by a recovery message before attempting a manual repair.

## Cloud sync

Open the context column with `Ctrl+J` and choose the cloud icon. It says what
Koru Cloud holds of **the document in front of you** — only that document, and
it follows you as you switch documents:

- **This document** — *Synced* when Koru Cloud holds this document as it is
  now, *Not synced yet* when it does not (it has never gone up, or you have
  changed it since it last did). *Not checked against Koru Cloud yet* means
  nothing has measured it since Koru started, and *Off, or not signed in on
  this device* means this Profile does not sync here.
- **Local** — when it last changed here, and whether that change came from this
  device, from a named device of yours, or from Koru Cloud.
- **Images** — how many of the pictures in this document Koru Cloud holds,
  *7 of 7 synced*. If this device is still missing some of them, the same line
  says how many and which device has them; pictures travel after the document,
  so one can still be on its way.
- **Files** — the same for everything else the document holds: PDFs,
  archives, spreadsheets.

A file counts as synced once Koru Cloud holds all of it; one still going up is
not. A document with no pictures shows no Images line, and one with no other
files shows no Files line.

How the whole Workspace is doing — how many of its documents and files are on
Koru Cloud, what is moving, and what is waiting for another device — is on
Manage's **Koru Cloud** page, under the Profile's **Show sync progress**.

Everything in this section is the **desktop's** context column; a phone shows
none of it, and says why a picture has not arrived where the picture is.

**Copy details** puts those same lines on the clipboard as plain text, with the
exact times rather than the abbreviated ones the column shows, so they can be
pasted into a message. `Ctrl+Alt+K` does the same without the pointer.

The section refreshes when the document changes and every five seconds while it
is showing; closing it, or the column, stops that entirely. Nothing it shows is
kept on disk — after a restart it is blank until the next pass runs.

**One thing about disk space, said once.** The first time Koru reads a picture
after this upgrade it writes a second copy into its new shared store and leaves
the original exactly where it was, so an older Koru can still open the same
content. Nothing removes either copy yet, so a workspace full of pictures takes
roughly twice the space it did until that changes.

## Every key

### Writing

| | |
| --- | --- |
| `Enter` | split the line into a new task |
| `Enter` (in a code block) | new line in the same block |
| `Ctrl+Enter` (in a code block) | leave the block — next paragraph, or make one |
| `Ctrl+Enter` (in a paragraph) | create a paragraph below without splitting the current one |
| `Alt+Enter` | create a paragraph above without splitting the current block |
| `Enter` (on a rendered table block) | create a paragraph below the complete table |
| `Ctrl+Enter` (in a table) | create a paragraph below the complete table |
| `Ctrl+Shift+Enter` | tick or untick the box on this line — a task or a checklist item; nothing on a line with no box |
| `Esc` (in table source) | validate and return to the rendered table |
| `Backspace` at the start | join into the line above |
| `Backspace` at the start of the first list item | convert it to a paragraph |
| `Ctrl+Shift+7` / `Ctrl+Shift+8` | toggle only the selected rows between numbered / bulleted list and paragraphs |
| `Ctrl+←` / `Ctrl+→` | move by word |
| `Shift+←` / `Shift+→` | select by character |
| `Ctrl+Shift+←` / `Ctrl+Shift+→` | select by word |
| `Ctrl+Backspace` / `Ctrl+Delete` | delete by word — a pill counts as one |
| `Ctrl+A` | select the current block; press it again to select the document body (not its title) |
| `Ctrl+C` / `Ctrl+X` | copy / cut — the selection, or the whole task |
| `Ctrl+V` | paste |
| `Ctrl+Shift+V` | paste as plain text — drop headings, marks and pictures |
| `Home` / `End` | start / end of the line |
| `Page Up` / `Page Down` | move the caret a page, scrolling smoothly; `Shift` extends the selection |
| `Esc` | dismiss a picker, or drop the selection |
| `Ctrl+Shift+D` (hold) | dictate — see [Dictating](#dictating) |

Deleting or cutting the full document-body selection leaves a fresh first
paragraph ready for typing. The title remains unchanged.

### Formatting

| | |
| --- | --- |
| `Ctrl+B` / `Ctrl+I` / `Ctrl+U` | bold / italic / underline |
| `Ctrl+Shift+C`, or select + `` ` `` | inline code |
| `Ctrl+` `` ` `` | toggle a code block |
| `Ctrl+Shift+S` | strikethrough |
| `Ctrl+Shift+H` | cycle the highlight color |
| `Ctrl+L` / `Ctrl+E` / `Ctrl+R` | align left / centre / right — Word's keys |
| `Ctrl+Alt+1` … `4` | heading 1–4 (documents only) |
| `Ctrl+Alt+0` | back to ordinary text |
| ``Ctrl+\`` | clear formatting, colors included |
| `Ctrl+.` | show the formatting bar at the caret (documents only) |

### Pills

| | |
| --- | --- |
| `/` | command menu |
| `@` | document entity picker |
| `Ctrl+K` | link |
| click a date pill | re-open its picker |
| click a document pill | follow it in this tab |
| `Ctrl+click` a document pill | open in a background tab (reuse if already open) |
| `Shift+click` a document pill | open in a new window |
| right-click a document pill | Open / Open in new tab / Open in new window |
| click a link pill | open it in the browser |
| `Ctrl+click` a link | open the URL without leaving Koru |
| hover a link | URL chip with copy and edit |
| click an attribute chip | open that attribute's picker for that block |
| click the `×` on any chip | take that one tag off, or clear that attribute (hover the chip to see it) — a dashed due chip has none, being a task with no date to clear |

### Structure

| | |
| --- | --- |
| `Tab` / `Shift+Tab` | indent / outdent |
| `Shift+Enter` | on a selected block (a divider, a picture), open a paragraph above it |
| `Ctrl+Shift+↑` / `↓` | move the task, or the selected block, up or down |
| `Shift+↑` / `Shift+↓` | select whole tasks |
| drag across rows | select whole tasks |
| `Ctrl+D`, `Delete`, `Backspace` | delete the task or selection; nested tasks outdent one level |
| drag the grip | move a task, re-parenting on drop |

### Everything else

| | |
| --- | --- |
| `Ctrl+P` / `Ctrl+Shift+F` | open global search inside Koru |
| `Ctrl+F` | find text in the current document |
| `Ctrl+H` | open local search with Replace expanded |
| `Ctrl+Alt+F` | alternate Replace shortcut when the keyboard layout delivers it |
| `Up` / `Down` (in local search) | previous / next match |
| `Enter` (in local replacement) | replace the active match and advance |
| `Ctrl+Enter` (in local search) | replace all current matches |
| `Ctrl+Shift+K` | open global search from anywhere on the machine (literal Ctrl on macOS too) |
| click the tray icon (Windows) | bring back the window you used last — or open global search when none is open |
| `Alt+↑` / `Alt+↓` (in Koru AI) | previous / next authored question; past the newest question returns to the composer |
| `Ctrl+Enter` (in search) | open the selected search result in a new window |
| `Ctrl+Shift+Enter` | tick or untick this line's box — a task or a checklist item |
| `Ctrl+Enter` | tick a task line; in a paragraph, table or code block it means what the Writing table says |
| `Ctrl+D` | delete the line; nested lines outdent |
| `Ctrl+Z` / `Ctrl+Shift+Z` | undo / redo |
| `F4` | show / hide completed |
| `F5` | sharing on / off |
| `F6` | move focus into the window's chrome — a standing reminder-recovery pill first, then the caption bell; `Esc` leaves it |
| `F7` | document only — page layout: Page → Narrow → Full |
| `Ctrl+Shift+P` `N` `G` | document only — Page, Narrow, Full width |
| `Ctrl+Shift+W` | close every tab — one welcome tab is left, never an empty window. Not listed in the tab menu |
| `Ctrl+N` | open another window on this workspace |
| `Alt+←` / `Alt+→` (macOS: `⌘[` / `⌘]`) | back / forward through the places you have been (or the mouse's thumb buttons) |
| click a document pill | follow it in this tab |
| `Ctrl+click` a document pill | open in a background tab |
| `Shift+click` a document pill | open in a new window |
| right-click a document pill | Open / Open in new tab / Open in new window |
| `→` onto a document pill, then `Enter` | open it — `Ctrl+Enter` in a background tab, `Shift+Enter` in a new window |
| drag a tab up / down | pop it out into its own window |
| drag a tab to a view's edge | split the window: the tab gets a new pane there |
| drag a tab onto another Koru window | move it there — an edge splits that window, the strip places it |
| drag a library row into a pane | open it there — an edge splits, the strip places it, the middle opens a tab |
| drag the hairline between panes | resize them (panes keep at least 400 × 200) |
| click below the last line | caret to the end of it |
| `Ctrl+Shift+T` | pin **this** window above other windows (or click 📌 in its caption) |
| `Ctrl+=` / `Ctrl+-` / `Ctrl+0` | zoom in / out / reset (or `Ctrl` + scroll) |

### Tabs and columns

Open tabs belong to the window and can remain selected while Explore shows a
different workspace. Images follow the selected tab: Koru reads, stores and
copies their bytes through that document's home workspace, including after a
restart.

| | |
| --- | --- |
| `Ctrl+T` | new tab (a blank page) |
| `Ctrl+Shift+A` | open **Add…** — create a document or import content |
| `F2` | rename the row you are pointing at, or the document showing |
| `Ctrl+click` a sidebar row | toggle one document in the selection |
| `Shift+click` a sidebar row | select the full range, including child rows |
| row `…` / right-click | **Open in new tab**, **Open in new window**, **Export → Markdown** or **TextBundle**, or move the selection to Trash |
| `↑` `↓` `←` `→` | walk the explore tree, once it has the keyboard |
| `Enter` | open the row the keyboard is on |
| `Ctrl+W` | close this tab — or the window, when it *is* one document |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | next tab / previous tab |
| `Ctrl+1` … `Ctrl+9` | the tab in that position |
| drag a tab | reorder it |
| right-click a tab → Move to New Window | move it into its own window |
| the two arrows in `LIBRARY` | fold / unfold the whole tree |
| `Ctrl+Alt+W` | the workspace switcher (without changing the Explore pane) |
| `Ctrl+Alt+P` | the profile switcher (without changing the Explore pane) |
| `Ctrl+Shift+E` | show / hide the pane (the logo does the same) — VS Code's Explorer key |
| `Ctrl+J` | show / hide the Context column |
| `Ctrl+Alt+J` | step the Context column to its next section (opens it first) |
| `Ctrl+Alt+K` | copy the Cloud sync section's facts to the clipboard |
| `Alt+1` / `Alt+2` / `Alt+3` | the Explore column's Documents, Files, Tasks |
| `Ctrl+Shift+J`, title-block sparkle, or **+ → AI Chat** | show / hide Koru AI in the right-hand pane slot (it replaces Context) |
| `Ctrl+Shift+I` | open or foreground the standalone Koru AI chat |

## Chatting with a local agent

Press **New chat** and open the model chip. Under **Local agents** are the
coding agents installed on this computer — Codex, OpenCode, Claude, Kimi,
Grok, Antigravity — each with a word when it is not ready (*Not installed*,
*Signed out*, *Needs a supported version*). Pick one and the chip reads its
name. Codex, OpenCode and Antigravity can also list their models: the small
arrow at the end of the row expands them (OpenCode groups them by provider),
and picking one puts it on the chip after the agent's name. Type and press
Enter.

**Antigravity** in a chat is Google's own connector for it, a separate
download from the `agy` command (Windows only). Run
`scripts\setup\fetch-antigravity-acp.ps1` once, then
`scripts\setup\fetch-antigravity-acp.ps1 -SignIn` to sign in with your Google
account in the browser, and restart Koru. Its first message takes about
twenty seconds to start. The **+** menu's Antigravity tab still opens `agy`
itself.

The agent answers in the chat, as Markdown. While it works, each tool it uses
is a row inside the turn with the agent's own words — *Working — Read
koru.bat* — and afterwards the row keeps only what kind of thing it was, how it
ended and when. When the agent asks permission, the row shows the choices it
offered; nothing happens until you answer, and a rejection is the safe answer.
**Stop** cancels the turn even while the agent is silent.

The agent runs on this computer with its own account and settings, in your
home folder, and stays alive while Koru runs. One agent conversation belongs
to one chat; once a chat has a message its agent cannot be swapped — start a
new chat instead. Deleting the chat ends the agent; if the agent could not be
stopped, the chat stays and says so, and you can delete it again. If the agent
had gone by the time you write again, Koru starts it fresh, hands it the
conversation so far, and tells you.

Before your first message, a folder button on the composer's toolbar (it
reads *Home folder* until you choose) opens a picker for the folder the
agent works in; the chosen folder is named in the header next to the title,
and a chat branched from this one keeps it. Once the first message is sent
the folder is fixed — start a new chat to work somewhere else.

**Effort.** For a model that offers levels — OpenAI's, the Koru model's
thinking, an Ollama model — the chip reads the model and its effort
(*gpt-5.6 · Medium*). Open the chip and drag or click the slider at the top
to a different level; the chat keeps that level and the next message uses
it. A local agent runs at its own effort and shows no slider.

In Settings, the **Local agents** card lists the same agents; saving one makes
it the default for new chats.

## Finding a chat again

Every chat is kept, and **Chats** in the left column (`Alt+4`, the sparkle
icon right of Tasks) lists them all, newest first. Each row says the chat's
title, who answered it — an agent by its name, and the model under it when you
picked one; the model for OpenAI and Ollama; *Qwen3 8B* for Koru — and how long
ago it last moved. A private chat wears the detective mark.

Click a row to open the chat as a tab; if a tab already shows it, that tab
comes forward. **New chat** at the top starts one. The list works from the
keyboard too: Up and Down move, Home and End jump to either end, Enter opens,
**Delete asks** before deleting, and Escape hands the keyboard back. Right-click
a row for **Delete chat**. If the chat's agent could not be stopped, the chat
stays and says so — delete it again.

In a chat tab, **Ctrl+Shift+E** (or the chat's own **Chats** button) opens the
list with that chat selected. A chat you start in a tab or in the Koru AI pane
appears in the list straight away.

In the standalone Koru AI window, **Chats** — or Ctrl+Shift+E — opens the same
list as a small floating pane. Choosing a row reads that chat in the window;
Escape or a click outside closes the pane.

### Mentioning a document with @

Type **@** at the start of a word in the chat's message box and pick a
document: it sits in your message as a chip, the same chip a page shows. The
list follows what you type, Up and Down move through it, Enter or Tab picks,
and Escape closes it and leaves what you typed. While the list is open, Enter
chooses rather than sends. Type `#` after a document's name to link one of its
headings. `@` inside a word — an email address — is just text.

When you send, each document you mentioned goes with the message and stays
pinned to the chat, and your message shows it by its title as a link; click it
to open the document. Up to 16 documents go with one message; Koru says so if
you mention more. Everything else in the box is plain text: Shift+Enter or
Ctrl+J starts a new line, `#` and `-` at the start of a line stay characters,
and a paste of several lines goes in where the caret is.

## Answering an agent's approval request

An agent may offer one-time permission, remembered permission, rejection, or
several choices with different scopes. Read the offered label and choose the
specific option you want. Use Tab or Shift+Tab to move between choices, then
Enter or Space to answer. Scroll to read long details and labels. Escape
closes the review without granting permission.

If the details are too large to review safely, granting choices are disabled;
you can still reject. An older connection shows its two original answers.
If those answers cannot describe the agent's offer, the request expires and
the turn stops instead of choosing a remembered permission for you. If the
agent offers no supported choice, the turn also stops. Koru explains that the
approval cannot be answered: update Koru or the agent, then try again.

## Retired agent choices

Gemini is no longer offered in the terminal **+** menu or **Delegate**. Older
run history is preserved. Antigravity opens as a tab and answers in a chat,
but is not a delegation choice.
