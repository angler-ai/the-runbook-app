REVOKE INSERT (sharing), UPDATE (sharing) ON credentials FROM rb_api;
GRANT SELECT ON proposals TO rb_vault;
GRANT UPDATE (status, decided_at, decided_by) ON proposals TO rb_vault;
