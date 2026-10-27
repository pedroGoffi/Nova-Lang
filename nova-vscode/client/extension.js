'use strict';
/** Minimal dependency-free VS Code client for NovaLS (Language Server Protocol). */
const vscode=require('vscode');
const {spawn}=require('node:child_process');
const path=require('node:path');

class RpcTransport {
  constructor(serverPath,output,onNotification) {
    this.output=output;this.onNotification=onNotification;
    this.pending=new Map();this.counter=1;this.buffer=Buffer.alloc(0);this.closed=false;
    this.proc=spawn(process.execPath,[serverPath],{env:{...process.env,ELECTRON_RUN_AS_NODE:'1'},stdio:['pipe','pipe','pipe'],windowsHide:true});
    this.proc.stdout.on('data',c=>this.read(c));
    this.proc.stderr.on('data',c=>output.append(c.toString('utf8')));
    this.proc.on('error',e=>{output.appendLine(`NovaLS launch failed: ${e.message}`);this.failAll(e);});
    this.proc.on('exit',(code,signal)=>{this.closed=true;this.failAll(new Error(`NovaLS exited: ${code??signal}`));output.appendLine(`NovaLS stopped (${code??signal})`);});
  }
  failAll(error){for(const [,entry] of this.pending){clearTimeout(entry.timeout);entry.reject(error);}this.pending.clear();}
  send(message){
    if(this.closed || !this.proc.stdin.writable)throw new Error('NovaLS is not running');
    const bytes=Buffer.from(JSON.stringify({jsonrpc:'2.0',...message}),'utf8');
    this.proc.stdin.write(Buffer.concat([Buffer.from(`Content-Length: ${bytes.length}\r\n\r\n`,'ascii'),bytes]));
  }
  notify(method,params){try{this.send({method,params});}catch(e){this.output.appendLine(String(e));}}
  request(method,params){
    if(this.closed)return Promise.reject(new Error('NovaLS stopped'));
    const id=this.counter++;
    return new Promise((resolve,reject)=>{
      const timeout=setTimeout(()=>{this.pending.delete(id);reject(new Error(`NovaLS request timed out: ${method}`));},30000);
      this.pending.set(id,{resolve,reject,timeout});
      try{this.send({id,method,params});}catch(error){clearTimeout(timeout);this.pending.delete(id);reject(error);}
    });
  }
  read(chunk){
    this.buffer=Buffer.concat([this.buffer,chunk]);
    while(this.buffer.length){
      const h=this.buffer.indexOf('\r\n\r\n');if(h<0)return;
      const match=this.buffer.subarray(0,h).toString('ascii').match(/(?:^|\r\n)Content-Length:\s*(\d+)/i);
      if(!match){this.buffer=this.buffer.subarray(h+4);continue;}
      const size=Number(match[1]);if(!Number.isSafeInteger(size)||size>16*1024*1024){this.buffer=Buffer.alloc(0);return;}
      if(this.buffer.length<h+4+size)return;
      const body=this.buffer.subarray(h+4,h+4+size).toString('utf8');
      this.buffer=this.buffer.subarray(h+4+size);
      try {this.dispatch(JSON.parse(body));}catch(error){this.output.appendLine(`NovaLS invalid response: ${error.message}`);}
    }
  }
  dispatch(message){
    if(Object.hasOwn(message,'id')){
      const entry=this.pending.get(message.id);if(!entry)return;
      this.pending.delete(message.id);clearTimeout(entry.timeout);
      if(message.error)entry.reject(new Error(message.error.message));else entry.resolve(message.result);
    }else if(message.method){this.onNotification(message.method,message.params||{});}
  }
  async stop(){
    if(this.closed)return;
    try{await this.request('shutdown',{});this.notify('exit',{});}catch{this.proc.kill();}
    this.closed=true;this.failAll(new Error('NovaLS stopped'));
    setTimeout(()=>{if(this.proc.exitCode===null)this.proc.kill();},750).unref();
  }
}

function lspPos(p){return new vscode.Position(p.line,p.character);}
function lspRange(r){return new vscode.Range(lspPos(r.start),lspPos(r.end));}
function lspSeverity(severity){return severity===2?vscode.DiagnosticSeverity.Warning:severity===3?vscode.DiagnosticSeverity.Information:severity===4?vscode.DiagnosticSeverity.Hint:vscode.DiagnosticSeverity.Error;}
const lspSymbols={3:vscode.SymbolKind.Namespace,5:vscode.SymbolKind.Class,10:vscode.SymbolKind.Enum,11:vscode.SymbolKind.Interface,12:vscode.SymbolKind.Function,23:vscode.SymbolKind.Struct,26:vscode.SymbolKind.TypeParameter};
const lspCompletions={3:vscode.CompletionItemKind.Function,7:vscode.CompletionItemKind.Class,14:vscode.CompletionItemKind.Keyword};
function enabled(doc){return doc && doc.languageId==='nova' && doc.uri.scheme==='file';}
function settings() {
  const c=vscode.workspace.getConfiguration('nova');
  return {compilerPath:c.get('compiler.path','nova'),onType:c.get('diagnostics.onType',true),onSave:c.get('diagnostics.onSave',true),debounceMs:c.get('diagnostics.debounceMs',400)};
}

async function activate(context){
  const output=vscode.window.createOutputChannel('Nova Language Server');
  const diagnostics=vscode.languages.createDiagnosticCollection('Nova');
  const status=vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left,40);
  status.text='$(check) NovaLS';status.tooltip='Nova Language Server';status.command='nova.showOutput';status.show();
  context.subscriptions.push(output,diagnostics,status);
  let rpc=null;let ready=false;let startPromise=null;
  const root=vscode.workspace.workspaceFolders?.[0]?.uri.toString()||null;
  const notification=(method,params)=>{
    if(method==='textDocument/publishDiagnostics'){
      const uri=vscode.Uri.parse(params.uri);
      const errors=(params.diagnostics||[]).map(item=>{
        const diagnostic=new vscode.Diagnostic(lspRange(item.range),item.message||'',lspSeverity(item.severity));
        diagnostic.source=item.source||'Nova';if(item.code!==undefined)diagnostic.code=item.code;
        return diagnostic;
      });
      diagnostics.set(uri,errors);
    }else if(method==='nova/status'){
      status.text=params.semanticReady?'$(check) NovaLS · Semântico':'$(warning) NovaLS · Sintaxe';
      status.tooltip=params.semanticReady?`Compilador Nova ativo: ${params.compiler}\nAnálise HIR / ownership / RAII / MIR`:`Compilador sem overlay LSP: ${params.compiler}\nSomente sintaxe durante a digitação. Atualize o compilador.`;
      output.appendLine(status.tooltip);
    }else if(method==='window/showMessage'){
      output.appendLine(params.message||'NovaLS message');
      if(params.type===1)vscode.window.showErrorMessage(params.message||'Nova language server error');
      else if(params.type===2)vscode.window.showWarningMessage(params.message||'Nova language server warning');
    }else if(method==='window/logMessage'){output.appendLine(params.message||'');}
  };
  async function start(){
    if(startPromise)return startPromise;
    startPromise=(async()=>{
      status.text='$(sync~spin) NovaLS';
      const serverFile=path.join(context.extensionPath,'server','novals.js');
      rpc=new RpcTransport(serverFile,output,notification);
      const initialized=await rpc.request('initialize',{processId:process.pid,rootUri:root,capabilities:{},initializationOptions:settings()});
      output.appendLine(`NovaLS ${initialized.serverInfo?.version||''} initialized`);
      rpc.notify('initialized',{});ready=true;status.text='$(check) NovaLS';
      for(const doc of vscode.workspace.textDocuments)if(enabled(doc))sendOpen(doc);
    })().catch(error=>{ready=false;status.text='$(error) NovaLS';output.appendLine(`NovaLS startup failed: ${error.stack||error}`);vscode.window.showErrorMessage(`NovaLS could not start: ${error.message}`);});
    return startPromise;
  }
  const registered=new Set();
  function sendOpen(doc){
    if(!ready||!enabled(doc))return;
    if(registered.has(doc.uri.toString()))return;
    registered.add(doc.uri.toString());
    rpc.notify('textDocument/didOpen',{textDocument:{uri:doc.uri.toString(),languageId:'nova',version:doc.version,text:doc.getText()}});
  }
  async function restart(){
    ready=false;registered.clear();diagnostics.clear();
    if(rpc)await rpc.stop();rpc=null;startPromise=null;
    await start();
  }
  context.subscriptions.push(
    vscode.workspace.onDidOpenTextDocument(doc=>sendOpen(doc)),
    vscode.workspace.onDidChangeTextDocument(evt=>{
      if(!ready||!enabled(evt.document))return;
      sendOpen(evt.document);
      rpc.notify('textDocument/didChange',{textDocument:{uri:evt.document.uri.toString(),version:evt.document.version},contentChanges:[{text:evt.document.getText()}]});
    }),
    vscode.workspace.onDidSaveTextDocument(doc=>{if(enabled(doc)&&ready){sendOpen(doc);rpc.notify('textDocument/didSave',{textDocument:{uri:doc.uri.toString()},text:doc.getText()});}}),
    vscode.workspace.onDidCloseTextDocument(doc=>{registered.delete(doc.uri.toString());if(ready&&enabled(doc))rpc.notify('textDocument/didClose',{textDocument:{uri:doc.uri.toString()}});diagnostics.delete(doc.uri);}),
    vscode.workspace.onDidChangeConfiguration(evt=>{if(evt.affectsConfiguration('nova')){if(ready)rpc.notify('workspace/didChangeConfiguration',{settings:settings()});}}),
    vscode.commands.registerCommand('nova.showOutput',()=>output.show(true)),
    vscode.commands.registerCommand('nova.restartServer',()=>restart()),
    vscode.commands.registerCommand('nova.check',async()=>{
      const doc=vscode.window.activeTextEditor?.document;
      if(!enabled(doc)){vscode.window.showInformationMessage('Open a .nova file to check it.');return;}
      await start();if(!ready)return;
      sendOpen(doc);rpc.notify('nova/check',{uri:doc.uri.toString()});
      output.appendLine(`Checking ${doc.uri.fsPath}...`);
    })
  );
  const selector=[{language:'nova',scheme:'file'}];
  async function request(method,doc,other={}){
    await start();if(!ready)return null;
    sendOpen(doc);
    try{return await rpc.request(method,{textDocument:{uri:doc.uri.toString()},...other});}
    catch(e){output.appendLine(`${method}: ${e.message}`);return null;}
  }
  context.subscriptions.push(
    vscode.languages.registerCompletionItemProvider(selector,{
      async provideCompletionItems(document,position){
        const data=await request('textDocument/completion',document,{position});
        return (data?.items||[]).map(item=>{
          const result=new vscode.CompletionItem(item.label,lspCompletions[item.kind]||vscode.CompletionItemKind.Text);
          result.detail=item.detail;return result;
        });
      }
    },':','.'),
    vscode.languages.registerHoverProvider(selector,{
      async provideHover(document,position){
        const data=await request('textDocument/hover',document,{position});
        if(!data)return null;
        const md=new vscode.MarkdownString(data.contents?.value||'');md.isTrusted=false;
        return new vscode.Hover(md);
      }
    }),
    vscode.languages.registerDefinitionProvider(selector,{
      async provideDefinition(document,position){
        const data=await request('textDocument/definition',document,{position});
        return data?new vscode.Location(vscode.Uri.parse(data.uri),lspRange(data.range)):null;
      }
    }),
    vscode.languages.registerDocumentSymbolProvider(selector,{
      async provideDocumentSymbols(document){
        const symbols=await request('textDocument/documentSymbol',document);
        return (symbols||[]).map(s=>new vscode.DocumentSymbol(s.name,s.detail||'',lspSymbols[s.kind]||vscode.SymbolKind.Variable,lspRange(s.range),lspRange(s.selectionRange)));
      }
    }),
    vscode.languages.registerDocumentFormattingEditProvider(selector,{
      async provideDocumentFormattingEdits(document,options){
        const edits=await request('textDocument/formatting',document,{options});
        return (edits||[]).map(e=>new vscode.TextEdit(lspRange(e.range),e.newText));
      }
    })
  );
  context.subscriptions.push({dispose(){ready=false;registered.clear();if(rpc){rpc.stop().catch(()=>{});} }});
  await start();
}
function deactivate(){}
module.exports={activate,deactivate};
