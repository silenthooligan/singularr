# Interface copy

Rules for every string a person sees in Singularr: the web client and anything the server sends for display (setting labels, check names, job lines, attention items, problem titles). The interface is a product for a worldwide audience. It uses the standard terms of its field and says as little as possible. It never explains itself, narrates, or addresses the reader in prose.

## Rules

1. **Labels are noun phrases** in the field's standard terms: Quality profile, Root folder, Release group, Indexer, Download client, Media server, Library, Subtitles, Notifications, Webhook, API key, Queue, History, Wanted, Monitored, Cutoff. Never a pronoun label (Its profile), never a sentence.
2. **Sentence case** everywhere. No period at the end of a label, button, column header, tab, badge, empty state or one-clause toast.
3. **Empty states are two to four words**: No pending requests. Queue is empty. No results. At most one action button. No reason, no reassurance, no instructions.
4. **Buttons are verbs**: Save, Test, Retry, Add channel, Clear filters, Send. A destructive button names what it destroys: Delete file.
5. **Errors are short and offer the way on**: Couldn't load Activity, with Retry. No route path, status code or internal name in the visible line. The technical detail may sit in a tooltip or a disclosure.
6. **Help text only where a control needs it**: one line, twelve words at most, about the setting. Not about how the product works, what it used to do, or what it will do later.
7. **Status is one or two words** from a fixed set: Available, Missing, Partial, Downloading, Importing, Processing, Queued, Working, Waiting, Stuck, Failed, Refused, Imported, Requested, Approved, Declined, Healthy, Degraded, Offline.
8. **Tooltips name the control** in six words or fewer: Row height, Columns, API request.
9. **Confirmations**: the title is the action as a question (Remove this request?), one line of consequence at most, buttons Cancel and the verb.
10. **A control that cannot act** is disabled and its tooltip says what it needs in a few words: Needs write access to Radarr. It does not open a toast that explains.
11. **Numbers and units**: 4.30 GB, 24%, 20 min, 3 d. Dates and times follow the workspace setting.
12. **One term per thing**, on every page. The glossary below is the list.

## Never

- "Singularr" as the subject of a sentence, outside About and permission dialogs.
- First or second person, questions to the reader outside confirmations, apologies, reassurance.
- "yet", "for now", "in this version", "stage", "round", "observing", "this installation".
- Explaining a number, a state or an empty list in a sentence under it.
- The name of this installation's own programs in product strings. A bridge is "the processing service" unless its configured name is shown as data.

## Glossary

| Use | Not |
|---|---|
| Quality profile | Profile, lane, its profile |
| Release group | Group, team |
| Media server | Server, Plex (unless shown as data) |
| Download client | Downloader, torrent client |
| Processing | Transcoding, conversion (as the section name) |
| Flow | Pipeline, job type |
| Run | Job (in processing) |
| Read-only | Observing |
| Write access | Act, acting |
| Request | Ask |
| Can be upgraded | Can be better, below the profile |
| Not released | Not out yet |
| Recording | Tape |
| Retention | Cleanup rules, lifecycle |

## Before and after

| Before | After |
|---|---|
| No request is waiting for a decision. | No pending requests |
| Its profile | Quality profile |
| The server did not answer /activity. | Couldn't load Activity (Retry) |
| Singularr reads your services and changes nothing on them yet. | Read-only |
| Nothing is done until a person asks for it. | (removed) |
| Send stages this file with HUSH. What the flow makes replaces it after the service's own checks. | Replaces the file when processing completes |
| Every event back to 22 Nov, 20:23 is loaded. | Loaded through 22 Nov 20:23 (Load older) |
| The newest 484 of 985 runs are loaded. | 484 of 985 loaded (Load all) |
| No library of the worker watches the queue this flow uses, so the file would only wait there. | No worker library for this flow |
| This title carries the tag hush-keep in its manager, which protects it from processing. | Protected by tag hush-keep |
| Rebuild audio has been waiting 8 d. No worker has a record of this file, so it can never finish. | Stuck 8 d, not in worker queue |
| The processing service answers | Processing service |
| Read the whole ledger: 1,019 jobs. | 1,019 runs |
