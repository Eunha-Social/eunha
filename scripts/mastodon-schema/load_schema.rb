# Execute Mastodon's db/schema.rb against a real Postgres database.
#
# schema.rb is an ActiveRecord DSL, not an application: loading it needs the
# schema DSL, the pg adapter, and Scenic for its two `create_view` calls —
# not Mastodon's app, its initializers, its secrets, or its full bundle.
require "active_record"

ActiveRecord::Base.establish_connection(ENV.fetch("DATABASE_URL"))
ActiveRecord::Base.logger = nil
ActiveRecord::Migration.verbose = false

# Mastodon uses Scenic for its two views. Scenic itself is a Railtie and would
# drag in the whole framework, so `create_view` is implemented here instead —
# the schema.rb calls pass their SQL inline, which is all it needs.
module ScenicStatements
  def create_view(name, version: nil, sql_definition: nil, materialized: false, **_opts)
    definition = sql_definition
    definition ||= raise(ArgumentError, "create_view #{name} needs a sql_definition")
    if materialized
      execute "CREATE MATERIALIZED VIEW #{quote_table_name(name)} AS #{definition}"
    else
      execute "CREATE VIEW #{quote_table_name(name)} AS #{definition}"
    end
  end
end
ActiveRecord::ConnectionAdapters::AbstractAdapter.include(ScenicStatements)

# Mastodon defines `timestamp_id` in Ruby (lib/mastodon/snowflake.rb) and
# references it from schema.rb as a column default, so it has to exist before
# the tables that use it. Taken verbatim from Mastodon's own source; the salt is
# per-installation and irrelevant to the schema's shape.
ActiveRecord::Base.connection.execute(<<~'FUNCTION'.sub(":random_string", "'schema-reference-salt'"))
CREATE OR REPLACE FUNCTION timestamp_id(table_name text)
RETURNS bigint AS
$$
  DECLARE
    time_part bigint;
    sequence_base bigint;
    tail bigint;
  BEGIN
    time_part := (
      -- Get the time in milliseconds
      ((date_part('epoch', now()) * 1000))::bigint
      -- And shift it over two bytes
      << 16);

    sequence_base := (
      'x' ||
      -- Take the first two bytes (four hex characters)
      substr(
        -- Of the MD5 hash of the data we documented
        md5(table_name || :random_string || time_part::text),
        1, 4
      )
    -- And turn it into a bigint
    )::bit(16)::bigint;

    -- Finally, add our sequence number to our base, and chop
    -- it to the last two bytes
    tail := (
      (sequence_base + nextval(table_name || '_id_seq'))
      & 65535);

    -- Return the time part and the sequence part. OR appears
    -- faster here than addition, but they're equivalent:
    -- time_part has no trailing two bytes, and tail is only
    -- the last two bytes.
    RETURN time_part | tail;
  END
$$ LANGUAGE plpgsql VOLATILE;
FUNCTION

# Every timestamp_id table draws from its own sequence, which Mastodon creates
# in `ensure_id_sequences_exist` rather than in schema.rb.
module SequenceStatements
  def create_table(name, **options, &block)
    if options[:id] == :bigint && options[:default].is_a?(Proc)
      execute "CREATE SEQUENCE IF NOT EXISTS #{quote_table_name("#{name}_id_seq")}"
    end
    super
  end
end
ActiveRecord::ConnectionAdapters::AbstractAdapter.prepend(SequenceStatements)

load ARGV.fetch(0)
puts "loaded #{ARGV.fetch(0)}"
