// The IronClaw and Cursor recipes on the Agents screen. The text comes from `shared/recipes.json`,
// the same file the README sections are written from (`scripts/recipes.mjs`), filled with what the
// owner typed. Nothing is read from the URL, no token is asked for, and nothing is sent anywhere:
// a recipe names the token file, never the token.
import source from '../../../shared/recipes.json';
import { renderBlocks } from '../../../shared/recipes.mjs';
import { IP4, LOCAL, problems, shq, type CommandFields } from './commands';

export type RecipeBlock =
  | { t: 'p'; text: string }
  | { t: 'ul'; items: string[] }
  | { t: 'code'; lang: string; title?: string; text: string };

export interface RecipeView {
  id: string;
  title: string;
  blocks: RecipeBlock[];
}

interface Recipe {
  id: string;
  title: string;
  blocks: unknown[];
}

// Only these fields reach a recipe; the funding key and amount belong to the command blocks.
const FIELDS = [
  'Agent name',
  'Token file path',
  'Agent user',
  'Socket or address',
  'Address',
  'Agent host',
];

const IRONCLAW_NEEDS_TCP =
  'IronClaw reaches nymirad over loopback TCP, so it needs a tcp token. Choose the tcp transport above to see its recipe.';
const CURSOR_NEEDS_SOCKET =
  'With a unix token the Cursor entry needs the path of the agent socket. Fill in the socket path above to see it.';

/** The recipes filled from the fields, or what is wrong with the fields. */
export function recipeViews(
  f: CommandFields,
): { ok: true; views: RecipeView[] } | { ok: false; errors: string[] } {
  const errors = problems(f).filter((e) => FIELDS.some((label) => e.startsWith(label)));
  if (errors.length > 0) return { ok: false, errors };
  const unix = f.transport === 'unix';
  const views = (source.recipes as Recipe[]).map((recipe): RecipeView => {
    if (recipe.id === 'ironclaw') {
      if (unix)
        return {
          id: recipe.id,
          title: recipe.title,
          blocks: [{ t: 'p', text: IRONCLAW_NEEDS_TCP }],
        };
      return {
        id: recipe.id,
        title: recipe.title,
        blocks: renderBlocks(recipe, {
          name: f.name,
          token_file: shq(f.tokenFile),
          endpoint: f.endpoint === '' ? `http://${LOCAL}:8788` : f.endpoint,
          local: LOCAL,
          agent_host: f.agentHost === '' ? 'AGENT_HOST' : shq(f.agentHost),
          agent_user: f.agentUser === '' ? 'AGENT_USER' : f.agentUser,
        }) as RecipeBlock[],
      };
    }
    const endpoint = unix ? f.endpoint : f.endpoint === '' ? `http://${IP4}:8788` : f.endpoint;
    if (unix && endpoint === '') {
      return {
        id: recipe.id,
        title: recipe.title,
        blocks: [{ t: 'p', text: CURSOR_NEEDS_SOCKET }],
      };
    }
    return {
      id: recipe.id,
      title: recipe.title,
      blocks: renderBlocks(recipe, {
        name: f.name,
        local: LOCAL,
        mcp: {
          installDir: '/opt/nymira-mcp',
          node: '/usr/local/bin/node',
          tokenFile: f.tokenFile,
          transport: f.transport,
          endpoint,
        },
      }) as RecipeBlock[],
    };
  });
  return { ok: true, views };
}
