// buildCompileContext (M4): exactly the CompileDirectory wire shape the wasm
// crate accepts (it denies unknown fields), every directory page followed, and
// an unreadable part reported as a gap instead of failing the editor.
import { http, HttpResponse } from 'msw';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { tokenStore } from '../../api/token-store';
import { server } from '../../test/msw/server';
import { problem } from '../editor/test-support';
import { buildCompileContext, latestSkills } from './context';

const HASH_A = 'aa'.repeat(32);
const HASH_B = 'bb'.repeat(32);
const HASH_C = 'cc'.repeat(32);

beforeEach(() => tokenStore.set('trb_pat_test'));
afterEach(() => tokenStore.clear());

describe('buildCompileContext', () => {
  it('reads every page and keeps only the fields the compiler takes', async () => {
    server.use(
      http.get('*/v1/orgs/:org/users', ({ request }) => {
        const cursor = new URL(request.url).searchParams.get('cursor');
        return HttpResponse.json(
          cursor
            ? {
                items: [{ id: 'usr_2', name: 'Bob', active: false, role: 'member' }],
                next_cursor: null,
              }
            : {
                items: [{ id: 'usr_1', name: 'Alice', active: true, role: 'owner' }],
                next_cursor: 'p2',
              },
        );
      }),
      http.get('*/v1/orgs/:org/teams', () =>
        HttpResponse.json({
          items: [{ id: 'team_1', slug: 'oncall', title: 'On call' }],
          next_cursor: null,
        }),
      ),
      http.get('*/v1/projects/:p/skills', () =>
        HttpResponse.json({
          items: [
            {
              name: 'rollback',
              hash: HASH_C,
              reference: null,
              description: 'd',
              files: [],
              usages: [],
              latest: true,
            },
            {
              name: 'errors',
              hash: HASH_A,
              reference: './skills/errors',
              description: 'd',
              files: [],
              usages: [],
              latest: false,
            },
            {
              name: 'errors',
              hash: HASH_B,
              reference: './skills/errors',
              description: 'd',
              files: [],
              usages: [],
              latest: true,
            },
          ],
          next_cursor: null,
        }),
      ),
      http.get('*/v1/projects/:p/vars', () =>
        HttpResponse.json({
          declarations: { region: { type: 'string' } },
          values: { region: 'eu' },
        }),
      ),
    );
    expect(await buildCompileContext('org_1', 'prj_1')).toEqual({
      directory: {
        users: [
          { id: 'usr_1', name: 'Alice', active: true },
          { id: 'usr_2', name: 'Bob', active: false },
        ],
        teams: [{ id: 'team_1', slug: 'oncall' }],
        skills: [
          { name: 'errors', hash: HASH_B, reference: './skills/errors' },
          { name: 'rollback', hash: HASH_C, reference: null },
        ],
        // Declarations only: values never reach the compiler.
        variables: { region: { type: 'string' } },
      },
      gaps: [],
    });
  });

  it('reports a forbidden skill library as a gap and fails on other errors', async () => {
    server.use(
      http.get('*/v1/orgs/:org/users', () => HttpResponse.json({ items: [], next_cursor: null })),
      http.get('*/v1/orgs/:org/teams', () => HttpResponse.json({ items: [], next_cursor: null })),
      http.get('*/v1/projects/:p/skills', () => problem(403, 'auth.scope_missing')),
      http.get('*/v1/projects/:p/vars', () => HttpResponse.json({ declarations: {}, values: {} })),
    );
    const context = await buildCompileContext('org_1', 'prj_1');
    expect(context.gaps).toEqual(['skills']);
    expect(context.directory.skills).toEqual([]);

    server.use(http.get('*/v1/projects/:p/vars', () => problem(503, 'storage.unavailable')));
    await expect(buildCompileContext('org_1', 'prj_1')).rejects.toMatchObject({
      code: 'storage.unavailable',
    });
  });

  it('pins each skill name to the version the server marks latest, wherever it is listed', () => {
    const view = (name: string, hash: string, latest: boolean) => ({
      name,
      hash,
      description: '',
      files: [],
      usages: [],
      latest,
    });
    // The catalog lists versions by `name:hash`, not by recency: the latest
    // `b` (HASH_C) comes after an older one, and `a` has only its latest.
    expect(
      latestSkills([view('b', HASH_A, false), view('a', HASH_B, true), view('b', HASH_C, true)]),
    ).toEqual([
      { name: 'a', hash: HASH_B, reference: null },
      { name: 'b', hash: HASH_C, reference: null },
    ]);
  });

  it('still names a skill whose versions carry no latest mark (the first listed one)', () => {
    const view = (hash: string) => ({
      name: 'errors',
      hash,
      description: '',
      files: [],
      usages: [],
      latest: false,
    });
    expect(latestSkills([view(HASH_A), view(HASH_B)])).toEqual([
      { name: 'errors', hash: HASH_A, reference: null },
    ]);
  });
});
