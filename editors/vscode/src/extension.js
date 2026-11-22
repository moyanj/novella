// novella VS Code 客户端插件：启动 novella-lsp 语言服务器。
//
// 客户端本身不含任何语法规则——高亮完全由服务器通过
// `textDocument/semanticTokens/full` 提供，避免词法规则双份维护。

const vscode = require('vscode');
const { LanguageClient, TransportKind } = require('vscode-languageclient/node');

/** @type {LanguageClient | undefined} */
let client;

/**
 * 读取用户配置，构造语言服务器的启动参数。
 *
 * 默认从 PATH 查找 `novella-lsp`；本地开发时可把 `novella.lsp.path`
 * 指向 `target/debug/novella-lsp`。
 */
function readServerOptions() {
  const config = vscode.workspace.getConfiguration('novella');
  return {
    command: config.get('lsp.path') || 'novella-lsp',
    args: config.get('lsp.args') || [],
    transport: TransportKind.stdio,
  };
}

/**
 * 构造客户端实例；清单文件变动经标准通知刷新目标相关诊断。
 * @param {import('vscode').FileSystemWatcher} manifestWatcher
 */
function createClient(manifestWatcher) {
  return new LanguageClient(
    'novella-lsp',
    'Novella Language Server',
    readServerOptions(),
    {
      documentSelector: [
        { scheme: 'file', language: 'novella' },
        { scheme: 'untitled', language: 'novella' },
      ],
      synchronize: { fileEvents: manifestWatcher },
      // 语义高亮由服务器 capabilities 声明的 legend 驱动，客户端无需额外注册
    },
  );
}

/**
 * 启动服务器；失败时给出可操作的提示，而不是静默无声。
 * @param {import('vscode').FileSystemWatcher} manifestWatcher
 */
async function startClient(manifestWatcher) {
  client = createClient(manifestWatcher);
  try {
    await client.start();
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    vscode.window.showErrorMessage(
      `novella 语言服务器启动失败：${detail}。请确认 novella.lsp.path 配置正确，或 PATH 中已安装 novella-lsp。`,
    );
  }
}

/** 停止服务器并释放实例。 */
async function stopClient() {
  const running = client;
  client = undefined;
  if (running) {
    await running.stop();
  }
}

/**
 * 重启服务器；重启后沿用同一个清单监听器，保留 `nov.toml` 变更同步。
 * @param {import('vscode').FileSystemWatcher} manifestWatcher
 */
async function restartClient(manifestWatcher) {
  await stopClient();
  await startClient(manifestWatcher);
}

async function activate(context) {
  const manifestWatcher = vscode.workspace.createFileSystemWatcher('**/nov.toml');
  context.subscriptions.push(manifestWatcher);
  await startClient(manifestWatcher);

  context.subscriptions.push(
    // 静态重启命令缺少清单监听器，包装成零参命令避免把命令参数当作 watcher。
    vscode.commands.registerCommand('novella.restartLanguageServer', () =>
      restartClient(manifestWatcher),
    ),
    // 配置变更后重启服务器，让 novella.lsp.path / args 立即生效
    vscode.workspace.onDidChangeConfiguration(async (event) => {
      if (!event.affectsConfiguration('novella.lsp')) {
        return;
      }
      await restartClient(manifestWatcher);
    }),
    new vscode.Disposable(() => {
      void stopClient();
    }),
  );
}

function deactivate() {
  return stopClient();
}

module.exports = { activate, deactivate };
