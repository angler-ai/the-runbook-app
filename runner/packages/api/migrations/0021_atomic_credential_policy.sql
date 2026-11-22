GRANT INSERT (id, org_id, name, type, config, binding, binding_hash, sharing, groups, delivery, created_by, label, label_initialized)
  ON credentials TO rb_vault;
GRANT UPDATE (name, config, binding, binding_hash, sharing, groups, delivery)
  ON credentials TO rb_vault;
