import { applyD1Migrations, env } from 'cloudflare:test';
import { shardBinding, shardCount } from '../src/shards.js';

// Every shard database gets the same schema.
for (let i = 0; i < shardCount(env); i++) await applyD1Migrations(shardBinding(env, i), env.TEST_MIGRATIONS);
