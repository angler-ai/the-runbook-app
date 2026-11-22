ALTER TABLE providers DROP CONSTRAINT providers_type_check;
ALTER TABLE providers ADD CONSTRAINT providers_type_check
  CHECK (type IN ('anthropic', 'openai', 'google', 'vertex', 'vertex-gemini', 'bedrock', 'azure-openai', 'openai-compatible'));
