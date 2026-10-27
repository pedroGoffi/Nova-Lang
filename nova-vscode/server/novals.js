#!/usr/bin/env node
'use strict';
/** Nova 0.20 Language Server v0.4. Uses the real Nova semantic pipeline with editor overlays. */
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { spawn } = require('node:child_process');
const { fileURLToPath, pathToFileURL } = require('node:url');

const KEYWORDS = ['fn','pub','let','mut','auto','struct','class','enum','trait','impl','concept','requires','where','extends','typedef','type','mod','import','use','namespace','macro','static_assert','return','if','else','while','loop','for','in','break','continue','match','async','await','gen','yield','coro','defer','unsafe','extern','const','virtual','override','new','true','false','next'];
const BUILTINS = ['i8','i16','i32','i64','u8','u16','u32','u64','f32','f64','bool','str','Self','std'];
const DECLARATION = /\b(?:(pub)\s+)?(fn|struct|class|enum|trait|concept|typedef|mod|namespace|macro)\s+([A-Za-z_][A-Za-z_0-9]*)/g;
const log = (...args) => console.error('[novals]',...args);

function asPath(uri) { try { return fileURLToPath(uri); } catch { return null; } }
function asUri(p) { return pathToFileURL(path.resolve(p)).toString(); }
function rootFor(file) {
  let current = path.dirname(file);
  for (;;) {
    if (fs.existsSync(path.join(current,'Nova.toml'))) return current;
    const parent = path.dirname(current);
    if (parent === current) return null;
    current = parent;
  }
}
function resolveCompiler(setting, workspace, alternateWorkspace=null) {
  const cmd = setting || 'nova';
  if (cmd !== 'nova') {
    if (path.isAbsolute(cmd)) return cmd;
    if (cmd.startsWith('.') || cmd.includes('/')) return path.resolve(workspace || process.cwd(),cmd);
    return cmd;
  }
  for (const location of [workspace,alternateWorkspace].filter(Boolean)) {
    for (const candidate of [path.join(location,'target','release','nova'),path.join(location,'target','debug','nova')]) {
      try { fs.accessSync(candidate,fs.constants.X_OK); return candidate; } catch { /* missing */ }
    }
  }
  return cmd;
}
function spawnCompiler(binary,args,cwd,timeoutMs=15000,env=process.env) {
  return new Promise(resolve => {
    let settled = false, stdout = '', stderr = '', truncated = false, timer;
    const finish = (result) => { if (!settled) {settled = true; clearTimeout(timer); resolve(result);} };
    let child;
    try { child = spawn(binary,args,{cwd,stdio:['ignore','pipe','pipe'],env,windowsHide:true}); }
    catch (e) { return resolve({ok:false,output:String(e),spawnError:true}); }
    timer = setTimeout(()=>{child.kill('SIGKILL');finish({ok:false,output:'Nova compiler timed out.',timedOut:true});},timeoutMs);
    const append = (type,buf) => {
      const remaining = 1024*1024 - Buffer.byteLength(stdout) - Buffer.byteLength(stderr);
      if (remaining<=0) {truncated=true;return;}
      const s=buf.toString('utf8'); const fragment = Buffer.byteLength(s) > remaining ? s.slice(0,remaining) : s;
      if(type==='stdout')stdout+=fragment;else stderr+=fragment;
    };
    child.stdout.on('data',buf=>append('stdout',buf));
    child.stderr.on('data',buf=>append('stderr',buf));
    child.on('error',e=>finish({ok:false,output:`Cannot start compiler '${binary}': ${e.message}`,spawnError:true}));
    child.on('close',code=>finish({ok:code===0,output:[stderr,stdout,truncated?'\n[output truncated]':''].filter(Boolean).join('\n'),code}));
  });
}
async function withScratch(content,work) {
  const dir=fs.mkdtempSync(path.join(os.tmpdir(),'novals-'));
  const file=path.join(dir,'source.nova');
  try {fs.writeFileSync(file,content,'utf8');return await work(file,dir);}
  finally {fs.rmSync(dir,{recursive:true,force:true});}
}
/** A private overlay snapshot is injected into Nova's normal file loader.
 * This keeps real source paths, Nova.toml dependencies, and all compiler passes,
 * without changing working tree contents or Nova.lock. The manifest is private
 * to the compiler subprocess and deleted immediately after it exits.
 */
async function withOverlays(documents,work) {
  const dir=fs.mkdtempSync(path.join(os.tmpdir(),'novals-overlay-'));
  try {
    const mapping=[];
    let index=0;
    for(const doc of documents){
      const original=asPath(doc.uri);
      if(!original)continue;
      if(original.includes('\t') || original.includes('\n'))continue;
      // A new .nova file may not exist on disk yet. The compiler's overlay
      // loader accepts it if its parent directory exists (never create paths).
      let canonical;
      try {
        canonical=fs.existsSync(original) ? fs.realpathSync(original) :
          path.join(fs.realpathSync(path.dirname(original)),path.basename(original));
      }catch{continue;}
      const replacement=path.join(dir,`buffer-${index++}.nova`);
      fs.writeFileSync(replacement,doc.text,{encoding:'utf8',mode:0o600});
      mapping.push(`${canonical}\t${replacement}`);
    }
    const manifest=path.join(dir,'overlays.map');
    fs.writeFileSync(manifest,mapping.join('\n')+'\n',{encoding:'utf8',mode:0o600});
    return await work(manifest);
  }finally{fs.rmSync(dir,{recursive:true,force:true});}
}
function stripAnsi(value){return String(value).replace(/\x1b\[[0-9;]*m/g,'');}
function readCaretWidth(lines,from) {
  for (let i=from+1;i<Math.min(from+6,lines.length);i++) {
    const caret=lines[i].match(/^\s*\d*\s*\|.*?(\^~*)\s*$/);
    if (caret) return caret[1].length;
  }
  return 1;
}
// AST expressions do not carry spans in Nova 0.20. For an unknown field,
// use the actual active function from the compiler trace as a strict bound.
// This does not invent a (0,0) location or scan unrelated modules.
function inferFieldToken(output,uri,text,relatedPath) {
  if (relatedPath && asPath(uri) && path.resolve(relatedPath)!==path.resolve(asPath(uri))) return null;
  const match=output.match(/(?:unknown field|'[^']+' has no field) '([A-Za-z_][A-Za-z_0-9]*)'/);
  const trace=output.match(/specializing function '([^']+)'/);
  if(!match||!trace||!text)return null;
  const field=match[1],fn=trace[1].split('::').at(-1);
  const masked=maskNonCode(text);
  if(!/^[A-Za-z_][A-Za-z_0-9]*$/.test(fn))return null;
  const declaration=new RegExp('\\bfn\\s+'+fn+'\\s*\\(','g');
  const fieldRx=new RegExp(`\\.\\s*(${field})\\b`,'g');
  const positions=[];
  let header;
  while((header=declaration.exec(masked))!==null){
    // The next function declaration ends the region for this method.
    const begin=header.index+header[0].length;
    const remaining=masked.slice(begin);
    const next=/\bfn\s+[a-zA-Z_][a-zA-Z_0-9]*\s*\(/.exec(remaining);
    const end=next?begin+next.index:masked.length;
    const region=masked.slice(header.index,end);
    let candidate;
    fieldRx.lastIndex=0;
    while((candidate=fieldRx.exec(region))!==null){
      const offset=header.index+candidate.index+candidate[0].lastIndexOf(field);
      positions.push(offset);
    }
  }
  if(positions.length!==1)return null;
  const at=positions[0],prefix=text.slice(0,at);
  const line=prefix.split('\n').length-1;
  const character=at-(prefix.lastIndexOf('\n')+1);
  return {start:{line,character},end:{line,character:character+field.length}};
}
function parseCompilerError(output,defaultUri,defaultText,cwd,sourceFile) {
  const cleaned=stripAnsi(output).trim();
  if (!cleaned) return [];
  const lines=cleaned.split(/\r?\n/);
  const heading=lines.find(line=>/\berror(?:\[[\w-]+\])?:|^nova:/.test(line)) || lines[0];
  const code=heading.match(/error\[([A-Za-z0-9-]+)\]/)?.[1] || 'NOVA';
  const title=heading.replace(/^.*?\berror(?:\[[\w-]+\])?:\s*/,'').replace(/^nova:\s*/,'').trim() || 'Nova compiler error';
  // A function-header location with this note is contextual ONLY.
  // Never render it as though it identified the failing expression.
  const contextualOnly = /function location only; failing expression has no precise AST span/.test(cleaned);
  const span=contextualOnly ? null : lines.map((line,i)=>({line,i,m:line.match(/^\s*-->\s+(.+?):(\d+):(\d+)\s*$/)})).find(x=>x.m);
  if (!span) {
    const related=lines.map(line=>line.match(/^\s*-->\s+(.+?):\d+:\d+\s*$/)).find(Boolean);
    const relative=related?.[1];
    const context=relative?(path.isAbsolute(relative)?relative:path.resolve(cwd||process.cwd(),relative)):null;
    const inferred=inferFieldToken(cleaned,defaultUri,defaultText,context);
    if(!inferred)return [];
    return [{uri:defaultUri,diagnostic:{range:inferred,severity:1,source:'Nova compiler',code,
      message:title+'\nLocal estimado exclusivamente dentro da função indicada pelo compilador.'}}];
  }

  let uri=defaultUri, row=0, col=0, width=1;
  if(span) {
    const location=span.m[1];
    if (!location.startsWith('<')) {
      if(sourceFile && path.resolve(location)===path.resolve(sourceFile)) uri=defaultUri;
      else uri=asUri(path.isAbsolute(location)?location:path.resolve(cwd||process.cwd(),location));
    }
    row=Math.max(0,Number(span.m[2])-1);
    col=Math.max(0,Number(span.m[3])-1);
    width=readCaretWidth(lines,span.i);
  }
  // Rust diagnostics use Unicode scalar columns; LSP uses UTF-16 offsets.
  let text=uri===defaultUri ? defaultText : undefined;
  if(text===undefined) {try {text=fs.readFileSync(asPath(uri),'utf8');}catch{text='';}}
  const line=(text||'').split(/\r?\n/)[row] || '';
  const scalarChars=Array.from(line);
  col=scalarChars.slice(0,col).join('').length;
  const end=Math.max(col+1,Math.min(line.length,col+Array.from(scalarChars.slice(0,Math.max(1,width)).join('')).length || 1));
  const details=lines.filter(l=>/^\s*= (?:help|note|compilation phase|package):/.test(l)).map(l=>l.trim()).slice(0,5).join('\n');
  const diagnostic={range:{start:{line:row,character:col},end:{line:row,character:end}},severity:1,source:'Nova compiler',code,message:title+(details?'\n'+details:'')};
  return [{uri,diagnostic}];
}
/** Nova lint runs the *same* semantic/ownership/MIR pipeline as `check`
 * and additionally emits warnings for unused/unnecessarily mutable locals.
 * Source spans are not yet retained by its warning pass. We attach a best-effort
 * location only if exactly one matching local declaration is found.
 */
function parseCompilerWarnings(output,documents,defaultUri){
  const warnings=[];
  const rx=/^warning\[(NOVA-L\d+)\]: '([^'\n]+)' in function '([^'\n]+)' (.+)$/gm;
  let match;
  while((match=rx.exec(stripAnsi(output)))!==null){
    const [,code,variable,fn,description]=match;
    const expression=new RegExp(`\\b(?:let|auto)\\s+(?:mut\\s+)?${variable.replace(/[.*+?^${}()|[\]\\]/g,'\\$&')}\\b`,'g');
    const locations=[];
    for(const doc of documents){
      const masked=maskNonCode(doc.text);
      let found;
      while((found=expression.exec(masked))!==null){
        const offset=found.index+found[0].lastIndexOf(variable);
        const preceding=doc.text.slice(0,offset),line=preceding.split('\n').length-1;
        const col=offset-(preceding.lastIndexOf('\n')+1);
        locations.push({uri:doc.uri,line,col});
      }
    }
    const exact=locations.length===1?locations[0]:null;
    warnings.push({uri:exact?.uri||defaultUri,diagnostic:{
      range:{start:{line:exact?.line||0,character:exact?.col||0},end:{line:exact?.line||0,character:exact?exact.col+variable.length:1}},
      severity:2,source:'Nova lint',code,
      message:`${variable} (${fn}): ${description}${exact?'':'\nO compilador ainda não fornece a posição exata deste aviso.'}`
    }});
  }
  return warnings;
}
function maskNonCode(text) {
  // Keep offsets (UTF-16 code units) unchanged while masking comments/strings.
  let result='', quoted=false, escaped=false, comment=false;
  for(let i=0;i<text.length;i++){
    const c=text[i];
    if(c==='\n'){result+='\n';comment=false;continue;}
    if(comment){result+=' ';continue;}
    if(quoted){
      result+=' ';
      if(escaped)escaped=false;
      else if(c==='\\')escaped=true;
      else if(c==='"')quoted=false;
      continue;
    }
    if(c==='/'&&text[i+1]==='/'){comment=true;result+='  ';i++;continue;}
    if(c==='"'){quoted=true;result+=' ';continue;}
    result+=c;
  }
  return result;
}
function declarationSymbols(text) {
  const result=[]; let match;
  const code=maskNonCode(text);
  const regex=new RegExp(DECLARATION.source,'g');
  while((match=regex.exec(code))!==null) {
    const prefix=text.slice(0,match.index);
    const line=prefix.split('\n').length-1;
    const character=prefix.length-(prefix.lastIndexOf('\n')+1);
    const nameStart=match[0].lastIndexOf(match[3]);
    const nameCol=character+nameStart;
    const range={start:{line,character},end:{line,character:character+match[0].length}};
    const selectRange={start:{line,character:nameCol},end:{line,character:nameCol+match[3].length}};
    const kind=({fn:12,struct:23,class:5,enum:10,trait:11,concept:11,typedef:26,mod:3,namespace:3,macro:12})[match[2]]||13;
    result.push({name:match[3],detail:match[2],kind,range,selectionRange:selectRange});
  }
  return result;
}
function wordAt(text,position) {
  const line=text.split(/\r?\n/)[position.line]||'';
  const start=Math.min(position.character,line.length);
  let from=start,to=start;
  while(from>0&&/\w/.test(line[from-1]))from--;
  while(to<line.length&&/\w/.test(line[to]))to++;
  return line.slice(from,to);
}
function safeDebounce(n) {return Math.max(100,Math.min(5000,Number(n)||400));}

class NovaServer {
  constructor(write,config={}) {
    this.write=write;
    this.documents=new Map();
    this.groups=new Map();
    this.compilerCapabilities=new Map();
    this.shutdownRequested=false;
    this.compilerWarningShown=false;
    this.config={compilerPath:'nova',onType:true,onSave:true,debounceMs:500,...config};
    this.workspace=null;
  }
  send(method,params){this.write({jsonrpc:'2.0',method,params});}
  respond(id,result,error){this.write(error?{jsonrpc:'2.0',id,error:{code:-32603,message:String(error)}}:{jsonrpc:'2.0',id,result});}
  settings(options){
    if (!options||typeof options!=='object')return;
    if(typeof options.compilerPath==='string')this.config.compilerPath=options.compilerPath;
    if(typeof options.onType==='boolean')this.config.onType=options.onType;
    if(typeof options.onSave==='boolean')this.config.onSave=options.onSave;
    if(options.debounceMs!==undefined)this.config.debounceMs=safeDebounce(options.debounceMs);
  }
  publish(uri,diagnostics){
    this.send('textDocument/publishDiagnostics',{uri,diagnostics});
  }
  groupFor(uri){
    const source=asPath(uri);
    if(!source)return null;
    return rootFor(source)||source;
  }
  groupState(key){
    if(!this.groups.has(key))this.groups.set(key,{seq:0,timer:null,targets:new Set()});
    return this.groups.get(key);
  }
  updateGroupDiagnostics(key,diagnostics,version){
    const group=this.groups.get(key);
    if(!group || (version!==undefined && version!==group.seq))return;
    const destination=new Map();
    for(const item of diagnostics){
      const existing=destination.get(item.uri)||[];
      existing.push(item.diagnostic);
      destination.set(item.uri,existing);
    }
    const opened=[...this.documents.values()].filter(doc=>this.groupFor(doc.uri)===key);
    const next=new Set([...destination.keys(),...opened.map(doc=>doc.uri)]);
    for(const uri of group.targets)if(!next.has(uri))this.publish(uri,[]);
    for(const uri of next)this.publish(uri,destination.get(uri)||[]);
    group.targets=next;
  }
  schedule(uri,kind,delay){
    const key=this.groupFor(uri);
    if(!key)return;
    const group=this.groupState(key);
    group.seq++;
    clearTimeout(group.timer);
    const seq=group.seq;
    group.timer=setTimeout(()=>this.validateGroup(key,kind,seq,uri).catch(e=>log('validation failure',e)),delay);
  }
  async overlayAvailable(binary,cwd){
    const cacheKey=binary+'\0'+cwd;
    if(!this.compilerCapabilities.has(cacheKey)){
      this.compilerCapabilities.set(cacheKey,(async()=>{
        const result=await spawnCompiler(binary,['lsp-probe'],cwd,5000);
        if(result.spawnError||result.timedOut){
          if(!this.compilerWarningShown){
            this.compilerWarningShown=true;
            this.send('window/showMessage',{type:2,message:result.output});
          }
          return false;
        }
        const available=result.ok && /nova-lsp-overlays-v[12]/.test(result.output);
        if(!available && !this.compilerWarningShown){
          this.compilerWarningShown=true;
          this.send('window/showMessage',{type:2,message:'Seu compilador Nova ainda não tem suporte a buffers LSP. Instale o compilador Nova atualizado junto com o NovaLS 0.4.0. Até lá, somente a sintaxe é validada durante a digitação; a checagem semântica usa arquivos salvos.'});
        }
        this.send('window/logMessage',{type:available?3:2,message:available?'NovaLS: full semantic analysis active (HIR, ownership/RAII, MIR)':'NovaLS: old compiler detected; semantic checking only on saved files'});
        this.send('nova/status',{semanticReady:available,compiler:binary});
        return available;
      })());
    }
    return this.compilerCapabilities.get(cacheKey);
  }
  async validateGroup(key,kind,seq,triggerUri){
    const group=this.groups.get(key);
    if(!group||group.seq!==seq)return;
    const trigger=this.documents.get(triggerUri)||[...this.documents.values()].find(doc=>this.groupFor(doc.uri)===key);
    if(!trigger)return;
    const source=asPath(trigger.uri);if(!source)return;
    const root=rootFor(source);
    const cwd=root||path.dirname(source);
    const compiler=resolveCompiler(this.config.compilerPath,root||this.workspace||cwd,this.workspace);
    const supportsOverlay=await this.overlayAvailable(compiler,cwd);
    if(group.seq!==seq)return;
    let result,reference=source;
    if(supportsOverlay){
      const snapshots=[...this.documents.values()].map(d=>({...d}));
      result=await withOverlays(snapshots,manifest=>spawnCompiler(compiler,['lint',root||source],cwd,45000,{...process.env,NOVA_LSP_OVERLAY_MAP:manifest}));
    }else if(kind==='syntax'){
      result=await withScratch(trigger.text,async filename=>{
        reference=filename;
        return spawnCompiler(compiler,['parse-rust',filename],cwd,12000);
      });
    }else{
      // Legacy compiler: accurate semantic checking is possible on saved sources only.
      result=await spawnCompiler(compiler,['check',root||source],cwd,45000);
    }
    if(group.seq!==seq)return;
    if(result.ok){this.updateGroupDiagnostics(key,supportsOverlay?parseCompilerWarnings(result.output,[...this.documents.values()],trigger.uri):[],seq);return;}
    if(result.spawnError||result.timedOut){
      this.send('window/showMessage',{type:1,message:`NovaLS não conseguiu executar o compilador: ${result.output}`});
      // Do not misrepresent a missing compiler as a source-code diagnostic.
      this.updateGroupDiagnostics(key,[],seq);
      return;
    }
    const diagnostics=parseCompilerError(result.output,trigger.uri,trigger.text,cwd,reference);
    if (!diagnostics.length) {
      // Unspanned error is still visible in the NovaLS output; do not attach
      // it to an unrelated line or overwrite another file's diagnostics.
      this.send('window/logMessage',{type:1,message:`Nova compiler error without source span:\n${result.output}`});
    }
    this.updateGroupDiagnostics(key,diagnostics,seq);
  }
  async format(uri) {
    const doc=this.documents.get(uri);if(!doc)return [];
    const file=asPath(uri);if(!file)return [];
    const root=rootFor(file);const cwd=root||path.dirname(file);
    const compiler=resolveCompiler(this.config.compilerPath,root||this.workspace||cwd,this.workspace);
    try {
      const formatted=await withScratch(doc.text,async filename=>{
        const result=await spawnCompiler(compiler,['fmt',filename],cwd);
        if(!result.ok) {log('formatter:',result.output);return null;}
        return fs.readFileSync(filename,'utf8');
      });
      if(formatted===null||formatted===doc.text)return [];
      const lines=doc.text.split('\n');const endLine=lines.length-1;
      return [{range:{start:{line:0,character:0},end:{line:endLine,character:lines[endLine].length}},newText:formatted}];
    }catch(e){log('formatter failed',e);return [];}
  }
  async receive(message) {
    const {id,method,params={}}=message;
    if(!method)return;
    try {
      switch(method){
        case 'initialize': {
          this.workspace=asPath(params.rootUri||'')||params.rootPath||null;
          this.settings(params.initializationOptions);
          this.respond(id,{capabilities:{textDocumentSync:{openClose:true,change:1,save:{includeText:true}},completionProvider:{resolveProvider:false,triggerCharacters:[':', '.']},hoverProvider:true,definitionProvider:true,documentSymbolProvider:true,documentFormattingProvider:true},serverInfo:{name:'NovaLS',version:'0.4.0'}});
          break;
        }
        case 'initialized':break;
        case 'workspace/didChangeConfiguration':this.settings(params.settings||params);this.compilerWarningShown=false;this.compilerCapabilities.clear();for(const uri of this.documents.keys())this.schedule(uri,'semantic',75);break;
        case 'textDocument/didOpen': {
          const input=params.textDocument;
          const doc={uri:input.uri,text:input.text,version:input.version||0};
          this.documents.set(doc.uri,doc);
          this.schedule(doc.uri,'semantic',100);
          break;
        }
        case 'textDocument/didChange': {
          const input=params.textDocument,doc=this.documents.get(input.uri);
          if(!doc)return;
          const changes=params.contentChanges||[];
          if(changes.length)doc.text=changes[changes.length-1].text;
          doc.version=input.version||doc.version+1;
          if(this.config.onType)this.schedule(doc.uri,'syntax',this.config.debounceMs);
          else { const key=this.groupFor(doc.uri); const group=key?this.groups.get(key):null; if(group){ group.seq++;clearTimeout(group.timer); } }
          break;
        }
        case 'textDocument/didSave': {
          const input=params.textDocument,doc=this.documents.get(input.uri);
          if(doc){
            if(typeof params.text==='string')doc.text=params.text;
            if(this.config.onSave)this.schedule(doc.uri,'semantic',50);
          }
          break;
        }
        case 'textDocument/didClose': {
          const uri=params.textDocument.uri;
          const key=this.groupFor(uri);
          this.documents.delete(uri);
          if(key){
            const remaining=[...this.documents.values()].find(d=>this.groupFor(d.uri)===key);
            if(remaining)this.schedule(remaining.uri,'semantic',50);
            else{
              const group=this.groups.get(key);
              if(group){clearTimeout(group.timer);group.seq++;for(const target of group.targets)this.publish(target,[]);this.groups.delete(key);}
            }
          }
          break;
        }
        case 'nova/check':this.schedule(params.uri,'semantic',0);if(id!==undefined)this.respond(id,null);break;
        case 'textDocument/completion': {
          const doc=this.documents.get(params.textDocument.uri);
          const symbols=doc?declarationSymbols(doc.text):[];
          const items=[...KEYWORDS.map(x=>({label:x,kind:14,detail:'Nova keyword'})),...BUILTINS.map(x=>({label:x,kind:7,detail:'Nova type / namespace'})),...symbols.map(x=>({label:x.name,kind:x.kind===12?3:7,detail:`${x.detail} · current file`}))];
          this.respond(id,{isIncomplete:false,items});break;
        }
        case 'textDocument/documentSymbol': {
          const doc=this.documents.get(params.textDocument.uri);
          this.respond(id,doc?declarationSymbols(doc.text):[]);break;
        }
        case 'textDocument/definition': {
          const doc=this.documents.get(params.textDocument.uri);
          const word=doc?wordAt(doc.text,params.position):'';
          const item=doc?declarationSymbols(doc.text).find(s=>s.name===word):null;
          this.respond(id,item?{uri:doc.uri,range:item.selectionRange}:null);break;
        }
        case 'textDocument/hover': {
          const doc=this.documents.get(params.textDocument.uri);
          const word=doc?wordAt(doc.text,params.position):'';
          const item=doc?declarationSymbols(doc.text).find(s=>s.name===word):null;
          const value=item?`**Nova ${item.detail}** \`${item.name}\`\n\nDeclaration in current file`:(KEYWORDS.includes(word)?`**Nova keyword** \`${word}\``:BUILTINS.includes(word)?`**Nova built-in** \`${word}\``:null);
          this.respond(id,value?{contents:{kind:'markdown',value}}:null);break;
        }
        case 'textDocument/formatting':this.respond(id,await this.format(params.textDocument.uri));break;
        case 'shutdown':this.shutdownRequested=true;this.respond(id,null);break;
        case 'exit':process.exit(this.shutdownRequested?0:1);break;
        default:if(id!==undefined)this.respond(id,null);
      }
    }catch(error){log(method,error);if(id!==undefined)this.respond(id,null,error?.message||error);}
  }
}
class JsonRpcReader {
  constructor(onMessage){this.buffer=Buffer.alloc(0);this.onMessage=onMessage;}
  feed(chunk) {
    this.buffer=Buffer.concat([this.buffer,Buffer.isBuffer(chunk)?chunk:Buffer.from(chunk)]);
    while(this.buffer.length){
      const headerEnd=this.buffer.indexOf('\r\n\r\n');
      if(headerEnd<0){if(this.buffer.length>16384)this.buffer=Buffer.alloc(0);return;}
      const header=this.buffer.subarray(0,headerEnd).toString('ascii');
      const match=header.match(/(?:^|\r\n)Content-Length:\s*(\d+)/i);
      if(!match){this.buffer=this.buffer.subarray(headerEnd+4);continue;}
      const n=Number(match[1]);
      if(!Number.isSafeInteger(n)||n>16*1024*1024){this.buffer=Buffer.alloc(0);return;}
      const end=headerEnd+4+n;
      if(this.buffer.length<end)return;
      const body=this.buffer.subarray(headerEnd+4,end).toString('utf8');
      this.buffer=this.buffer.subarray(end);
      try{this.onMessage(JSON.parse(body));}catch(e){log('bad jsonrpc input',e.message);}
    }
  }
}
function encodeRpc(value){const body=Buffer.from(JSON.stringify(value),'utf8');return Buffer.concat([Buffer.from(`Content-Length: ${body.length}\r\n\r\n`,'ascii'),body]);}
if(require.main===module){
  const server=new NovaServer(message=>process.stdout.write(encodeRpc(message)));
  const reader=new JsonRpcReader(message=>{void server.receive(message);});
  process.stdin.on('data',data=>reader.feed(data));
  process.stdin.on('end',()=>process.exit(0));
  process.on('uncaughtException',error=>log('uncaught exception',error));
}
module.exports={NovaServer,JsonRpcReader,encodeRpc,parseCompilerError,parseCompilerWarnings,declarationSymbols,rootFor,resolveCompiler,withScratch,withOverlays,spawnCompiler,wordAt};
