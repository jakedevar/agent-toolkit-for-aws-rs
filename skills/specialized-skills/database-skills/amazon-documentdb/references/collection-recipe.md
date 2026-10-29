# Database statistics collection recipe (two-step)

The legacy `wa_review.py` connected to the database itself. The toolkit has no database
driver, no shell launcher and never receives a connection string: the toolkit's
connection-URI and TLS-override flags stay refused. Collection is two steps, and
only step 1 touches the database:

1. **You** run a short list of read-only commands in **your own** authenticated
   `mongosh` and paste their canonical Extended JSON replies into a private
   capture file (this page).
2. `toolkit documentdb normalize-capture` validates the capture offline and writes
   the `--analysis-data` file that `toolkit documentdb wa-review` reads.

Nothing here is a script. There is no loop, helper or generated file: every command
below is one expression you run by hand. The machine-readable allowlist is
[collection-manifest.json](collection-manifest.json); a Rust test keeps this page, the
manifest and the normalizer equal.

## Cost of doing it by hand

The work is proportional to the deployment: two listing commands, then **one `collStats` reply, one `$indexStats` reply and one `getIndexes` reply
for every collection** in every database other than `admin`, `local` and `config`. A
deployment with hundreds of collections takes hundreds of manual steps. A future
automated snippet would be executable shipped text and needs its own no-write review;
it is not part of this recipe. If that cost is too high, run `wa-review` without
`--analysis-data`: the infrastructure checks still run.

## Supported shells

| Shell | Support |
| --- | --- |
| `mongosh` 1.x and 2.x | supported: `EJSON.stringify(value, { relaxed: false })` prints canonical Extended JSON (`{"$numberLong": "..."}`), so 64-bit counts are exact |
| legacy `mongo` shell | unsupported: it has no canonical export and prints `NumberLong(...)`; such a capture is refused (`unsupported_format`) |

Relaxed Extended JSON (bare numbers) is also refused: it would round 64-bit counts.
Engines exercised by the offline fixtures: Amazon DocumentDB 5.0 and 8.0 and MongoDB
7.0. Fixtures were reconstructed from the documented reply shapes; the live check
against real engines is the operator step T-DG-09d.

## Custody (read before you start)

- Authenticate interactively. Give `mongosh` only non-secret selectors and let it
  ask for the password, which it prompts for when `--username` is given without a
  password:

  ```text
  mongosh --tls --host HOST:27017 --tlsCAFile global-bundle.pem --username USER --authenticationDatabase admin
  ```

  Never put the password or a connection string on a command line, in shell history,
  in a script, in the capture or in any file. If your secure tooling uses a connection
  string, keep it in that tooling's private credential store. The recipe never
  retrieves, prints or stores one.
- Create a private directory and capture before you paste anything:

  ```text
  umask 077
  mkdir -m 700 docdb-capture
  install -m 600 /dev/null docdb-capture/capture.json
  ```

  Directory mode 0700 and file mode 0600 (owner only). The toolkit never creates the
  capture. `normalize-capture` refuses a group- or world-accessible capture before
  it parses it, and refuses where permissions cannot be verified. Its output is
  created `0600` and never overwrites a file.
- Paste only replies. Do not paste shell transcripts, prompts, connection details or
  error messages. A field outside the allowlist, or anything credential- or
  connection-shaped, is refused with a fixed error class that does not repeat it.

## Commands

Run each row in `mongosh` (replace `DATABASE` and `COLLECTION`). `list_databases` and
`list_collections` give you the names to type into the envelope. The other three
print canonical Extended JSON; paste it into the slot shown.

<!-- manifest-rows:begin -->
| Id | Command (run exactly one at a time) | Slot |
| --- | --- | --- |
| `list_databases` | `db.adminCommand({ listDatabases: 1, nameOnly: true })` | `databases[].name` |
| `list_collections` | `db.getSiblingDB("DATABASE").runCommand({ listCollections: 1, nameOnly: true })` | `databases[].collections[].name` |
| `coll_stats` | `EJSON.stringify(db.getSiblingDB("DATABASE").runCommand({ collStats: "COLLECTION" }), { relaxed: false })` | `databases[].collections[].collStats` |
| `index_stats` | `EJSON.stringify(db.getSiblingDB("DATABASE").getCollection("COLLECTION").aggregate([{ $indexStats: {} }]).toArray(), { relaxed: false })` | `databases[].collections[].indexStats` |
| `list_indexes` | `EJSON.stringify(db.getSiblingDB("DATABASE").getCollection("COLLECTION").getIndexes(), { relaxed: false })` | `databases[].collections[].indexes` |
<!-- manifest-rows:end -->

All five are read-only. No sampling of documents, no index change and no write is
needed. Fields the normalizer reads: `collStats`: `count`, `size`, `avgObjSize`,
`storageSize`, `totalIndexSize`, `indexSizes`, `compression.enabled` or
`compression.enable`. `$indexStats`: `name`, `accesses.ops`. `getIndexes`: `name`,
`key` (field order is kept), `expireAfterSeconds` (zero is a TTL; `null` is none).
A few harmless scalars of real replies (`ns`, `ok`, `capped`, `nindexes`, `host`,
`accesses.since` and similar, listed as `ignored` in the manifest) may stay in; any
other field, such as `wiredTiger`, must be removed from the pasted reply.

## The envelope

```json
{
  "format": "toolkit-docdb-collection-capture",
  "version": 1,
  "dialect": "mongosh-ejson-canonical",
  "shell": {"name": "mongosh", "version": "2.3.1"},
  "engine": {"name": "documentdb", "version": "5.0"},
  "databases": [
    {"ordinal": 1, "name": "admin"},
    {"ordinal": 2, "name": "app", "collections": [
      {"ordinal": 1, "name": "users",
       "collStats": { "...": "paste the coll_stats reply" },
       "indexStats": [ "paste the index_stats reply" ],
       "indexes": [ "paste the list_indexes reply" ]},
      {"ordinal": 2, "name": "denied", "error": "unauthorized"}
    ]}
  ]
}
```

- Per collection: `collections` hold `name`, `collStats`, `indexStats` and `indexes`
  (or `error`).
- `format`, `version` and `dialect` are fixed. `shell` and `engine` are optional
  records of what you used (`name` and `version`).
- `ordinal` numbers databases (and collections within a database) from 1 in the
  order you list them, with no gaps; this catches a skipped or duplicated entry.
  `admin`, `local`, `config` and `system.*` collections are accepted and ignored.
- A collection you could not read: give it an `error` instead of the three replies,
  one of the fixed codes `connection_failed`, `authentication_failed`, `unauthorized`,
  `command_failed`, `unsupported_command` or `timeout`. `normalize-capture` keeps the
  marker, prints an "incomplete" count, and `wa-review` skips that collection. A
  database or the whole capture with an `error` (a listing or connection failure)
  publishes no snapshot at all. Never paste driver error text.
- `databases` may not be empty of usable collections: a capture where every
  collection failed is refused (`empty`), so a failed run cannot pass for a healthy one.

## Normalize, then review

```text
toolkit documentdb normalize-capture --capture docdb-capture/capture.json --output docdb-capture/analysis.json
toolkit documentdb wa-review --cluster-id ID --region REGION --analysis-data docdb-capture/analysis.json
```

Refusals print `capture refused (<class>): <fixed sentence>` and exit 1:
`unreadable`, `too_large`, `permissions`, `malformed`, `unsupported_format`,
`unknown_field`, `secret_field`, `invalid_number`, `invalid_error_code`, `incomplete`,
`empty`, `output_exists`, `output_failed`. The sentence never repeats a value, name or
key of your capture.

Keep the capture and the analysis file private, delete them when done, and do not
commit or upload them: collection and index names are customer metadata.
