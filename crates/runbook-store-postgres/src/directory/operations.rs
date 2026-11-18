use super::*;
use runbook_core::directory_operations::*;

fn db<T>(_: T) -> DirectoryError {
    DirectoryError::Backend("postgres.directory_operation")
}
impl DirectoryOperations for PostgresDirectory {
    async fn lookup_replay(
        &self,
        org: &OrganizationId,
        expected_revision: u64,
        replay: &RequestReplay,
    ) -> Result<ReplayResult, DirectoryError> {
        let mut c = self.pool.get().await.map_err(db)?;
        c.begin(READ_COMMITTED).await.map_err(db)?;
        let result =
            async {
                let row = c.client().query_opt(
                "SELECT revision FROM runbook.directory_revision WHERE org_id=$1 FOR UPDATE",
                &[&org.as_str()],
            ).await.map_err(db)?.ok_or(DirectoryError::NotFound)?;
                if uint(row.get(0))? != expected_revision {
                    return Ok(ReplayResult::Conflict);
                }
                let now = time(c.client()).await?;
                lookup(c.client(), org.as_str(), replay, now).await
            }
            .await;
        // Read-only probe: release the revision lock on every outcome.
        c.rollback().await;
        result
    }

    async fn current_time_ms(&self) -> Result<i64, DirectoryError> {
        let c = self.pool.get().await.map_err(db)?;
        time(c.client()).await
    }
    async fn project_variables(
        &self,
        org: &OrganizationId,
        project: &ProjectId,
    ) -> Result<VariablesRecord, DirectoryError> {
        let c = self.pool.get().await.map_err(db)?;
        if !project_in_org(c.client(), org.as_str(), project).await? {
            return Err(DirectoryError::NotFound);
        }
        let row=c.client().query_opt("SELECT body,revision FROM runbook.project_variables WHERE org_id=$1 AND project_id=$2",&[&org.as_str(),&project.as_str()]).await.map_err(db)?;
        row.map(|r| {
            Ok(VariablesRecord {
                variables: serde_json::from_str(r.get(0)).map_err(db)?,
                revision: uint(r.get(1))?,
            })
        })
        .transpose()
        .map(|v| v.unwrap_or_default())
    }
    async fn apply_operation(
        &self,
        operation: DirectoryOperation,
    ) -> Result<OperationResult, DirectoryError> {
        let mut c = self.pool.get().await.map_err(db)?;
        c.begin(READ_COMMITTED).await.map_err(db)?;
        let result = apply(c.client(), operation).await;
        match result {
            Ok(
                OperationResult::Conflict
                | OperationResult::KeyReused
                | OperationResult::VariablesConflict,
            ) => {
                c.rollback().await;
                result
            }
            _ => finish(&mut c, result).await,
        }
    }
}
async fn time(c: &Client) -> Result<i64, DirectoryError> {
    Ok(c.query_one(
        "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint",
        &[],
    )
    .await
    .map_err(db)?
    .get(0))
}
async fn apply(c: &Client, op: DirectoryOperation) -> Result<OperationResult, DirectoryError> {
    let org = op.commit.org_id.as_str();
    let old: i64 = c
        .query_opt(
            "SELECT revision FROM runbook.directory_revision WHERE org_id=$1 FOR UPDATE",
            &[&org],
        )
        .await
        .map_err(db)?
        .ok_or(DirectoryError::NotFound)?
        .get(0);
    if uint(old)? != op.commit.expected_revision {
        return Ok(OperationResult::Conflict);
    }
    let now = time(c).await?;
    if let Some(r) = &op.replay {
        match lookup(c, org, r, now).await? {
            ReplayResult::Applied(response) => return Ok(OperationResult::Applied(response)),
            ReplayResult::KeyReused => return Ok(OperationResult::KeyReused),
            ReplayResult::Missing => {}
            ReplayResult::Conflict => return Ok(OperationResult::Conflict),
        }
    }

    if op.commit.mutations.is_empty() {
        if op.variables.is_none() {
            return Err(DirectoryError::Invalid("empty operation"));
        }
        let next = old.checked_add(1).ok_or(DirectoryError::RevisionOverflow)?;
        c.execute(
            "UPDATE runbook.directory_revision SET revision=$2 WHERE org_id=$1",
            &[&org, &next],
        )
        .await
        .map_err(db)?;
    } else if super::apply(c, op.commit.clone()).await? == CommitResult::Conflict {
        return Ok(OperationResult::Conflict);
    }
    if let Some(v) = &op.variables {
        if !project_in_org(c, org, &v.project_id).await? {
            return Err(DirectoryError::NotFound);
        }
        let old = c
            .query_opt(
                "SELECT revision FROM runbook.project_variables WHERE org_id=$1 AND project_id=$2",
                &[&org, &v.project_id.as_str()],
            )
            .await
            .map_err(db)?
            .map_or(0, |r| r.get::<_, i64>(0));
        if uint(old)? != v.expected_revision {
            return Ok(OperationResult::VariablesConflict);
        }
        let next = old.checked_add(1).ok_or(DirectoryError::RevisionOverflow)?;
        let body = serde_json::to_string(&v.variables).map_err(db)?;
        c.execute("INSERT INTO runbook.project_variables(org_id,project_id,body,revision) VALUES($1,$2,$3,$4) ON CONFLICT(project_id) DO UPDATE SET body=EXCLUDED.body,revision=EXCLUDED.revision WHERE runbook.project_variables.org_id=EXCLUDED.org_id",&[&org,&v.project_id.as_str(),&body,&next]).await.map_err(db)?;
    }
    if let Some(r) = &op.replay {
        c.execute(
            "DELETE FROM runbook.directory_request WHERE org_id=$1 AND expires_at<=$2",
            &[&org, &now],
        )
        .await
        .map_err(db)?;
        let payload = serde_json::to_string(&r.payload).map_err(db)?;
        let response = serde_json::to_string(&op.response).map_err(db)?;
        c.execute("INSERT INTO runbook.directory_request(org_id,principal,operation,key,payload,response,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7)",&[&org,&r.principal,&r.operation,&r.key,&payload,&response,&now.saturating_add(86_400_000)]).await.map_err(db)?;
    }
    Ok(OperationResult::Applied(op.response))
}

pub(crate) async fn lookup(
    c: &Client,
    org: &str,
    r: &RequestReplay,
    now: i64,
) -> Result<ReplayResult, DirectoryError> {
    let row = c.query_opt(
        "SELECT payload,response FROM runbook.directory_request WHERE org_id=$1 AND principal=$2 AND operation=$3 AND key=$4 AND expires_at>$5",
        &[&org, &r.principal, &r.operation, &r.key, &now],
    ).await.map_err(db)?;
    if let Some(row) = row {
        let payload: serde_json::Value = serde_json::from_str(row.get(0)).map_err(db)?;
        return Ok(if payload == r.payload {
            ReplayResult::Applied(serde_json::from_str(row.get(1)).map_err(db)?)
        } else {
            ReplayResult::KeyReused
        });
    }
    Ok(ReplayResult::Missing)
}
