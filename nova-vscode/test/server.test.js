'use strict';
const test=require('node:test');const assert=require('node:assert/strict');
const fs=require('node:fs'),os=require('node:os'),path=require('node:path');
const {spawn}=require('node:child_process');const {pathToFileURL}=require('node:url');
const {JsonRpcReader,encodeRpc,parseCompilerError,declarationSymbols,rootFor,resolveCompiler}=require('../server/novals');

const mock=path.join(__dirname,'mock-nova.js');
async function fixture(fn){
  const root=fs.mkdtempSync(path.join(os.tmpdir(),'novals-test-'));
  fs.mkdirSync(path.join(root,'src'));fs.writeFileSync(path.join(root,'Nova.toml'),'[package]\nname = "demo"\nversion = "0.1.0"\n');
  const file=path.join(root,'src','main.nova');fs.writeFileSync(file,'fn main() -> i32 { return 0; }\n');
  try{return await fn({root,file,uri:pathToFileURL(file).toString()});}
  finally{fs.rmSync(root,{force:true,recursive:true});}
}

class Peer {
  constructor(options={}){
    this.proc=spawn(process.execPath,[path.join(__dirname,'../server/novals.js')],{stdio:['pipe','pipe','pipe'],env:{...process.env,...options.env}});
    this.pending=new Map();this.notifications=[];this.waiting=[];this.counter=1;
    this.reader=new JsonRpcReader(message=>this.dispatch(message));
    this.proc.stdout.on('data',data=>this.reader.feed(data));
    this.stderr='';this.proc.stderr.on('data',data=>{this.stderr+=data.toString();});
  }
  dispatch(message){
    if(message.id!==undefined){const cb=this.pending.get(message.id);if(cb){this.pending.delete(message.id);cb(message);}}
    else{
      this.notifications.push(message);
      for(const entry of [...this.waiting])if(entry.predicate(message)){
        this.waiting.splice(this.waiting.indexOf(entry),1);clearTimeout(entry.timer);entry.resolve(message);
      }
    }
  }
  write(value){this.proc.stdin.write(encodeRpc({jsonrpc:'2.0',...value}));}
  request(method,params={}){return new Promise((resolve,reject)=>{
    const id=this.counter++;const t=setTimeout(()=>{this.pending.delete(id);reject(new Error('timeout '+method+'\n'+this.stderr));},5000);
    this.pending.set(id,message=>{clearTimeout(t);resolve(message.result);});
    this.write({id,method,params});
  });}
  notify(method,params){this.write({method,params});}
  waitFor(predicate){
    const existing=this.notifications.find(predicate);if(existing)return Promise.resolve(existing);
    return new Promise((resolve,reject)=>{const entry={predicate,resolve};entry.timer=setTimeout(()=>{this.waiting.splice(this.waiting.indexOf(entry),1);reject(new Error('timed out for notification\n'+this.stderr+'\n'+JSON.stringify(this.notifications)));},6000);this.waiting.push(entry);});
  }
  async end(){try{await this.request('shutdown');this.notify('exit');}catch{this.proc.kill();}this.proc.kill();}
}

test('JSON-RPC frames support multi-byte UTF-8 and partial reads',()=>{
  const received=[];const reader=new JsonRpcReader(x=>received.push(x));
  const encoded=encodeRpc({jsonrpc:'2.0',method:'test',params:{value:'Olá 🌎'}});
  reader.feed(encoded.subarray(0,9));reader.feed(encoded.subarray(9,28));reader.feed(encoded.subarray(28));
  assert.equal(received.length,1);assert.equal(received[0].params.value,'Olá 🌎');
});
test('semantic diagnostic uses compiler file, line and Unicode-correct column',()=>{
  const u='file:///tmp/main.nova';
  const data="error[NOVA-T001]: unknown 'x'\n  --> /tmp/main.nova:1:2\n1 | 😀x\n  |   ^\n  = help: define x";
  const found=parseCompilerError(data,u,'😀x','/tmp','/tmp/main.nova');
  assert.equal(found[0].uri,u);assert.equal(found[0].diagnostic.range.start.character,2);
  assert.equal(found[0].diagnostic.code,'NOVA-T001');
});
test('unspanned semantic error never creates a false line-zero marker',()=>{
  const uri='file:///tmp/response.nova';
  const code="impl Response { fn status(self: Response) -> Response { self.to = 1; return self; } }";
  const output="error[NOVA-T001]: 'Response' has no field 'to'\n  = note: semantic AST has no precise source span for this error";
  assert.deepEqual(parseCompilerError(output,uri,code,'/tmp','/tmp/response.nova'),[]);
});
test('related function declaration is not the failing expression',()=>{
  const uri='file:///tmp/response.nova';
  const output="error[NOVA-T001]: 'Response' has no field 'to'\nerror[NOVA-T001]: related function declaration\n  --> /tmp/response.nova:1:1\n  = note: function location only; failing expression has no precise AST span";
  assert.deepEqual(parseCompilerError(output,uri,'fn status() {}','/tmp','/tmp/response.nova'),[]);
});
test('field assignment points to self.to in Response::status, not line zero',()=>{
  const uri='file:///tmp/response.nova';
  const text=`pub struct Response {descriptor: i32}
impl Response {
 fn status(self: Response) -> Response {
  self.to = 1;
  return self;
 }
}`;
  const output="error[NOVA-T001]: 'Response' has no field 'to'\n  = note: semantic AST has no precise source span for this error\nerror[NOVA-T001]: related function declaration\n  --> /tmp/response.nova:3:2\n  = note: function location only; failing expression has no precise AST span\n  = compiler trace (function specialization, innermost first):\n    0: specializing function 'response::Response::status'";
  const found=parseCompilerError(output,uri,text,'/tmp','/tmp/response.nova');
  assert.equal(found.length,1);
  assert.deepEqual(found[0].diagnostic.range.start,{line:3,character:7});
  assert.deepEqual(found[0].diagnostic.range.end,{line:3,character:9});
});
test('field location ambiguity is not guessed',()=>{
  const uri='file:///tmp/response.nova';
  const text='fn status(self: Response) -> Response { self.to = 1; self.to = 2; return self; }';
  const out="error[NOVA-T001]: 'Response' has no field 'to'\n  = nova-trace: specializing function 'Response::status'";
  assert.deepEqual(parseCompilerError(out,uri,text,'/tmp','/tmp/response.nova'),[]);
});
test('symbols and manifest discovery',()=>fixture(async({file,root})=>{
  assert.equal(rootFor(file),root);
  assert.equal(declarationSymbols('pub fn add() -> i32 {}\nstruct User { id: i32 }').length,2);
  assert.deepEqual(declarationSymbols('// fn fake()\nlet x = \"struct Fake\";\nfn real() -> i32 {}').map(s=>s.name),['real']);
  assert.equal(resolveCompiler('/tmp/bin/nova',root),'/tmp/bin/nova');
}));
test('LSP real process: unsaved full semantic, syntax, formatting, navigation',()=>fixture(async({file,uri,root})=>{
  const peer=new Peer();
  try {
    const init=await peer.request('initialize',{processId:process.pid,rootUri:pathToFileURL(root).toString(),capabilities:{},initializationOptions:{compilerPath:mock,onType:true,onSave:true,debounceMs:100}});
    assert.ok(init.capabilities.documentFormattingProvider);
    peer.notify('initialized',{});
    peer.notify('textDocument/didOpen',{textDocument:{uri,languageId:'nova',version:1,text:'fn main() -> i32 { return 0; }\n'}});
    await peer.waitFor(m=>m.method==='textDocument/publishDiagnostics'&&m.params.uri===uri&&m.params.diagnostics.length===0);
    const invalid='fn main() -> i32 {\n    BAD_SYNTAX\n}\n';
    peer.notifications=[];
    peer.notify('textDocument/didChange',{textDocument:{uri,version:2},contentChanges:[{text:invalid}]});
    const syntax=await peer.waitFor(m=>m.method==='textDocument/publishDiagnostics'&&m.params.uri===uri&&m.params.diagnostics[0]?.code==='NOVA-P001');
    assert.equal(syntax.params.diagnostics[0].range.start.line,1);
    assert.equal(syntax.params.diagnostics[0].range.start.character,4);
    assert.equal(syntax.params.uri,uri);
    const fixed='fn main() -> i32 {\n    return BAD_TYPE;\n}\n';
    fs.writeFileSync(file,fixed);
    peer.notify('textDocument/didChange',{textDocument:{uri,version:3},contentChanges:[{text:fixed}]});
    peer.notifications=[];
    peer.notify('textDocument/didSave',{textDocument:{uri},text:fixed});
    const semantic=await peer.waitFor(m=>m.method==='textDocument/publishDiagnostics'&&m.params.uri===uri&&m.params.diagnostics[0]?.code==='NOVA-T001');
    assert.equal(semantic.params.diagnostics[0].range.start.line,1);
    const completion=await peer.request('textDocument/completion',{textDocument:{uri},position:{line:0,character:0}});
    assert.ok(completion.items.some(i=>i.label==='async'));
    const symbols=await peer.request('textDocument/documentSymbol',{textDocument:{uri}});
    assert.equal(symbols[0].name,'main');
    const def=await peer.request('textDocument/definition',{textDocument:{uri},position:{line:0,character:3}});
    assert.equal(def.uri,uri);
    const hover=await peer.request('textDocument/hover',{textDocument:{uri},position:{line:0,character:3}});
    assert.ok(hover.contents.value.includes('Nova fn'));
    peer.notify('textDocument/didChange',{textDocument:{uri,version:4},contentChanges:[{text:'fn main() -> i32 {  \n    return 0;\n}\n'}]});
    const fmt=await peer.request('textDocument/formatting',{textDocument:{uri},options:{tabSize:4,insertSpaces:true}});
    assert.equal(fmt.length,1);assert.ok(!fmt[0].newText.includes('{  '));
    peer.notify('textDocument/didClose',{textDocument:{uri}});
  }finally{await peer.end();}
}));


test('unsaved owners, conflicting borrows and RAII pass through full compiler check',()=>fixture(async({file,uri,root})=>{
  const original=fs.readFileSync(file,'utf8');
  const peer=new Peer();
  try{
    await peer.request('initialize',{rootUri:pathToFileURL(root).toString(),initializationOptions:{compilerPath:mock,onType:true,debounceMs:100}});
    peer.notify('textDocument/didOpen',{textDocument:{uri,languageId:'nova',version:1,text:original}});
    await peer.waitFor(m=>m.method==='textDocument/publishDiagnostics'&&m.params.uri===uri&&m.params.diagnostics.length===0);
    for(const [i,marker,expected] of [[2,'MOVED_OWNER','moved value'],[3,'BAD_BORROW','mutable borrow'],[4,'BAD_RAII','owned value']]){
      const draft=`fn main() -> i32 {\n  ${marker}\n  return 0;\n}\n`;
      peer.notifications=[];
      peer.notify('textDocument/didChange',{textDocument:{uri,version:i},contentChanges:[{text:draft}]});
      const found=await peer.waitFor(m=>m.method==='textDocument/publishDiagnostics' && m.params.uri===uri && m.params.diagnostics.some(d=>d.message.includes(expected)));
      assert.equal(found.params.diagnostics[0].range.start.line,1);
      assert.equal(fs.readFileSync(file,'utf8'),original,'No saved source overwritten by LSP');
      assert.equal(fs.existsSync(path.join(root,'Nova.lock')),false,'Nova.lock not touched by LSP');
    }
    peer.notifications=[];
    peer.notify('textDocument/didChange',{textDocument:{uri,version:5},contentChanges:[{text:original}]});
    await peer.waitFor(m=>m.method==='textDocument/publishDiagnostics'&&m.params.uri===uri&&m.params.diagnostics.length===0);
  }finally{await peer.end();}
}));

test('all unsaved module buffers are mapped into the same semantic check',()=>fixture(async({file,uri,root})=>{
  const second=path.join(root,'src','other.nova'),secondUri=pathToFileURL(second).toString();
  fs.writeFileSync(second,'pub fn value() -> i32 { return 0; }\n');
  const peer=new Peer();
  try{
    await peer.request('initialize',{rootUri:pathToFileURL(root).toString(),initializationOptions:{compilerPath:mock,onType:true,debounceMs:100}});
    peer.notify('textDocument/didOpen',{textDocument:{uri,languageId:'nova',version:1,text:fs.readFileSync(file,'utf8')}});
    peer.notify('textDocument/didOpen',{textDocument:{uri:secondUri,languageId:'nova',version:1,text:fs.readFileSync(second,'utf8')}});
    peer.notifications=[];
    peer.notify('textDocument/didChange',{textDocument:{uri:secondUri,version:2},contentChanges:[{text:'pub fn value() -> i32 { BAD_BORROW return 0; }'}]});
    const found=await peer.waitFor(m=>m.method==='textDocument/publishDiagnostics' && m.params.uri===secondUri&& m.params.diagnostics.some(d=>d.message.includes('borrow')));
    assert.equal(found.params.uri,secondUri);
    assert.ok(!fs.readFileSync(second,'utf8').includes('BAD_BORROW'));
  }finally{await peer.end();}
}));


test('legacy compiler degrades to parser on edit without pretending to check owners',()=>fixture(async({file,uri,root})=>{
  const peer=new Peer({env:{NOVA_MOCK_LEGACY:'1'}});
  try{
    await peer.request('initialize',{rootUri:pathToFileURL(root).toString(),initializationOptions:{compilerPath:mock,onType:true,debounceMs:100}});
    peer.notify('textDocument/didOpen',{textDocument:{uri,languageId:'nova',version:1,text:fs.readFileSync(file,'utf8')}});
    await peer.waitFor(m=>m.method==='textDocument/publishDiagnostics'&&m.params.uri===uri&&m.params.diagnostics.length===0);
    peer.notifications=[];
    peer.notify('textDocument/didChange',{textDocument:{uri,version:2},contentChanges:[{text:'fn main() -> i32 { MOVED_OWNER return 0; }'}]});
    const message=await peer.waitFor(m=>m.method==='textDocument/publishDiagnostics'&&m.params.uri===uri);
    assert.deepEqual(message.params.diagnostics,[],'Old compiler cannot check ownership on unsaved text');
    peer.notifications=[];
    peer.notify('textDocument/didChange',{textDocument:{uri,version:3},contentChanges:[{text:'fn main() -> i32 { BAD_SYNTAX return 0; }'}]});
    const syntax=await peer.waitFor(m=>m.method==='textDocument/publishDiagnostics'&&m.params.uri===uri&&m.params.diagnostics.some(d=>d.code==='NOVA-P001'));
    assert.equal(syntax.params.diagnostics[0].code,'NOVA-P001');
  }finally{await peer.end();}
}));

test('lint warnings from the compiler are published with severity Warning on unsaved local',()=>fixture(async({uri,root,file})=>{
  const peer=new Peer();
  try{
    await peer.request('initialize',{rootUri:pathToFileURL(root).toString(),initializationOptions:{compilerPath:mock,onType:true,debounceMs:100}});
    const text='fn main() -> i32 {\n  let unused: i32 = 10;\n  return 0;\n}\n';
    peer.notify('textDocument/didOpen',{textDocument:{uri,languageId:'nova',version:1,text}});
    const found=await peer.waitFor(m=>m.method==='textDocument/publishDiagnostics'&&m.params.uri===uri&&m.params.diagnostics.some(d=>d.code==='NOVA-L001'));
    const diagnostic=found.params.diagnostics.find(d=>d.code==='NOVA-L001');
    assert.equal(diagnostic.severity,2);
    assert.equal(diagnostic.range.start.line,1);
    assert.equal(diagnostic.range.start.character,6);
    assert.ok(!fs.readFileSync(file,'utf8').includes('unused'));
  }finally{await peer.end();}
}));


test('new unsaved declared module is checked without creating the source file',()=>fixture(async({file,uri,root})=>{
  const fresh=path.join(root,'src','fresh.nova'),freshUri=pathToFileURL(fresh).toString();
  const peer=new Peer();
  try {
    await peer.request('initialize',{rootUri:pathToFileURL(root).toString(),initializationOptions:{compilerPath:mock,onType:true,debounceMs:100}});
    peer.notify('textDocument/didOpen',{textDocument:{uri,languageId:'nova',version:1,text:'mod fresh;\nfn main() -> i32 { return 0; }\n'}});
    peer.notify('textDocument/didOpen',{textDocument:{uri:freshUri,languageId:'nova',version:1,text:'pub fn helper() -> i32 { BAD_BORROW return 0; }'}});
    const found=await peer.waitFor(m=>m.method==='textDocument/publishDiagnostics'&&m.params.uri===freshUri&&m.params.diagnostics.some(d=>d.message.includes('borrow')));
    assert.equal(found.params.diagnostics[0].range.start.line,0);
    assert.equal(fs.existsSync(fresh),false,'The LSP must not create a module on disk');
    assert.equal(fs.readFileSync(file,'utf8'),'fn main() -> i32 { return 0; }\n');
    assert.equal(fs.existsSync(path.join(root,'Nova.lock')),false);
  }finally{await peer.end();}
}));
