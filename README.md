Eunha
=====

Rust re-implementation of Mastodon.

Eunha aims for 100% Mastodon database schema compatibility, so that Eunha can be a drop-in replacement on top of your existing Mastodon database.

We track the latest Mastodon release, and provide migration path from old Eunha database schema to updated Mastodon database schema.

It's not Eunha's goal to completely mimic Mastodon's feature set or its implementation detail, and Eunha may contain behavioral differences.


Contributing
------------

All Mastodon tables should go in the `public` schema, while tables needed for Eunha goes in the `eunha` schema.

Use mise for all tasks. See `mise.toml`.

Use [shadcn/ui](https://ui.shadcn.com) CLI when adding components. Don't hand-roll components.


Federation
----------

For all federation related tasks, we use [feder](https://github.com/limeburst/feder), and extend it when necessary.
