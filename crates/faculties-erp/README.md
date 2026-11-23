# Erlkönig ERP

This crate provides the ERP command and the bookkeeping adapter for Erlkönig. The command uses an explicit ERP collection descriptor for ordinary reads and writes. Bookkeeping operations that import originals also require the selected Files collection.

## Revolver

Print the ordered PostgreSQL COPY query for one table:

```sh
erlkonig erp source revolver import --show-sql --table l_261
```

For an existing workspace, supply its ERP descriptor. To try the importer in a disposable pile, run the following in an empty directory. Save the descriptor printed by the first command as `TRIBLESPACE_COLLECTION_ERP`.

```sh
export PILE="$PWD/demo.pile"
erlkonig erp source revolver init
```

The following example creates a synthetic five-column PostgreSQL COPY capture and imports it twice. The second run reports zero changed records.

```sh
mkdir -p sample
printf '1\tExample Operator\tEO\tExample\tOperator\n' > sample/userlist.copy
erlkonig erp source revolver import --table userlist --from-dir sample --as-of '2030-01-02 09:00:00'
erlkonig erp source revolver import --table userlist --from-dir sample --as-of '2030-01-02 09:00:00'
erlkonig erp source revolver census
```

For database reads, the importer starts `psql` locally or on the host supplied with `--ssh`. Use `--database` to select the database. `--mandant` selects one tenant for tables with tenant ownership. The user directory is shared. Offline replay reads the same ordered fields from `<table>.copy`. Apply tenant and deletion filters when producing the capture. `--dry-run` decodes and validates rows before publication.

Installations can bind five optional logical fields with repeated `--field FIELD=COLUMN` arguments.

| Field | Meaning |
| --- | --- |
| `levy_amount` | Purchase-line levy amount |
| `levy_amount_c` | Source currency selector for the levy. Bind together with `levy_amount`. |
| `levy_rate` | Purchase-line levy rate |
| `task_contact` | Task contact reference |
| `task_step` | Task step reference |

For example, `--field levy_amount=f_example_fee --field levy_amount_c=f_example_fee_c` selects both fields from that installation. Unbound fields occupy their original COPY positions as SQL NULL. When the selected tenant and table already have a current value for an optional field, live import asks for its mapping before writing. A mapped column can still report an actual SQL NULL. Offline captures retain any values already present in those positions. Use `--currency-code CODE=ISO` to supply currency selector mappings when needed.

## Bookkeeping and finance

`erlkonig erp bookkeeping --help` lists account, Amex CSV, invoice, review, classification and matching operations. German Amex activity exports use the 11-column format or its extended 13-column cardholder variant. A CSV import preserves the original bytes under the Files route and records the explicitly selected reporting month. The account or cardholder mapping is supplied by the operator.

`erlkonig erp finance --help` lists operations for recording attributed observations, listing them and reconciling cash movements. The library also exposes the DATEV EXTF Buchungsstapel decoder.

The package is available under the MIT or Apache-2.0 license.
