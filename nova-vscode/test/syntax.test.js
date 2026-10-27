'use strict';
const test=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const root=path.resolve(__dirname,'..');
const grammar=JSON.parse(fs.readFileSync(path.join(root,'syntaxes/nova.tmLanguage.json'),'utf8'));
const pkg=JSON.parse(fs.readFileSync(path.join(root,'package.json'),'utf8'));

function patternsOf(repo){return grammar.repository[repo].patterns;}
function matchScope(repo,scope,text){
 return patternsOf(repo).some(pattern=>pattern.name===scope && pattern.match && new RegExp(pattern.match).test(text));
}
function walkPatterns(patterns){
 for(const pattern of patterns){
  if(pattern.include) assert.ok(grammar.repository[pattern.include.slice(1)],`Missing include ${pattern.include}`);
  for(const key of ['match','begin','end']) if(pattern[key])assert.doesNotThrow(()=>new RegExp(pattern[key]),`Invalid regex: ${pattern[key]}`);
  if(pattern.patterns)walkPatterns(pattern.patterns);
 }
}
function capturedScope(repo,scope,name,text){
 return patternsOf(repo).some(p=>p.name===scope && p.match && (()=>{
  const m=new RegExp(p.match).exec(text);
  return m && Object.entries(p.captures||{}).some(([index,config])=>config.name===name && m[Number(index)]);
 })());
}

test('VSCodium grammar and publisher remain backward compatible with 0.4.0 LSP',()=>{
 assert.equal(pkg.name,'nova-language-tools');assert.equal(pkg.publisher,'merchonline');
 assert.equal(pkg.version,'0.5.1');assert.equal(pkg.main,'./client/extension.js');
 assert.ok(pkg.contributes.languages[0].extensions.includes('.nova'));
 assert.equal(pkg.contributes.grammars[0].scopeName,'source.nova');
 assert.equal(grammar.scopeName,'source.nova');
});
test('regex validity / recursive scopes',()=>{
 walkPatterns(grammar.patterns);
 for(const value of Object.values(grammar.repository))walkPatterns(value.patterns);
});
test('function definitions, generators, async, extern and declarations',()=>{
 const fn='meta.function.definition.nova';
 for(const text of ['pub async fn listen(port: i32)', 'gen fn ticks()', 'coro fn scheduler()', 'fn main()', 'extern fn puts(text: str)']){
  assert.ok(matchScope('definitions',fn,text),text);
 }
 assert.ok(capturedScope('definitions',fn,'entity.name.function.definition.nova','fn add(a: i32)'));
 for(const kind of ['struct','class','enum','trait','concept','typedef'])
  assert.ok(matchScope('definitions','meta.definition.type.nova',`${kind} Widget {}`),kind);
 assert.ok(matchScope('definitions','meta.definition.namespace.nova','namespace std {'));
 assert.ok(matchScope('definitions','meta.definition.namespace.nova','mod json;'));
 assert.ok(matchScope('definitions','meta.definition.macro.nova','macro component(name:ident)'));
 assert.ok(matchScope('definitions','meta.definition.variable.nova','let mut resource = thing;'));
 assert.ok(matchScope('definitions','meta.definition.variable.nova','auto count = 5;'));
 assert.ok(matchScope('definitions','meta.definition.associated-type.nova','type Data;'));
});
test('types, literals, operators, borrowing, resources and coroutines',()=>{
 for(const op of ['yield','await','next','finished','resume','join','spawn','dispose']){
  assert.ok(matchScope('keywords','keyword.control.coroutine.nova',`${op} worker`),op);
  assert.ok(!matchScope('keywords','keyword.control.coroutine.nova',`obj.${op}()`),`method ${op}`);
 }
 for(const kind of ['defer','unsafe'])assert.ok(matchScope('keywords','keyword.control.resource.nova',kind));
 for(const kind of ['usize','isize','i32','f64','bool','str','char'])
  assert.ok(matchScope('types','storage.type.primitive.nova',`value: ${kind}`),kind);
 assert.ok(matchScope('keywords','constant.language.boolean.nova','return true;'));
 assert.ok(matchScope('operators','keyword.operator.range.nova','0..100'));
 assert.ok(!matchScope('operators','keyword.operator.range.nova','...values'));
 assert.ok(matchScope('operators','keyword.operator.spread.nova','...values'));
 assert.ok(matchScope('operators','keyword.operator.function.nova','->'));
 assert.ok(matchScope('operators','keyword.operator.function.nova','=>'));
 assert.ok(matchScope('operators','keyword.operator.access.nova','std::json'));
 assert.ok(matchScope('numbers','constant.numeric.float.nova','3.14'));
 assert.ok(matchScope('numbers','constant.numeric.integer.nova','0..100'));
 assert.ok(matchScope('numbers','constant.numeric.integer.nova','0..100'.slice(3)));
});
test('methods, properties, namespace, receiver, macro and function calls',()=>{
 assert.ok(matchScope('callsAndMembers','entity.name.function.member.nova','cursor.next()'));
 assert.ok(matchScope('callsAndMembers','variable.other.property.nova','user.id'));
 assert.ok(capturedScope('callsAndMembers','meta.macro.invocation.nova','entity.name.function.macro.nova','component!("x")'));
 assert.ok(matchScope('callsAndMembers','entity.name.namespace.nova','std::json::Foo'));
 assert.ok(matchScope('callsAndMembers','support.type.user.nova','User::fromJson(text)'));
 assert.ok(matchScope('callsAndMembers','entity.name.function.call.nova','fromJson(text)'));
 assert.ok(matchScope('callsAndMembers','variable.language.self.nova','self.name'));
 assert.ok(matchScope('callsAndMembers','variable.parameter.nova','count: &i32'));
});
test('attributes support #[...] and [[...]] and keep token grouping',()=>{
 assert.ok(patternsOf('attributes').some(p=>new RegExp(p.begin).test('#[derive(Clone)]')));
 assert.ok(patternsOf('attributes').some(p=>new RegExp(p.begin).test('[[unsafe]]')));
 assert.ok(patternsOf('attributes').some(p=>p.patterns.some(x=>x.name==='support.type.derive.nova')));
});
function luminance(hex){
 const rgb=[1,3,5].map(i=>parseInt(hex.slice(i,i+2),16)/255);
 const [r,g,b]=rgb.map(v=>v<=0.04045?v/12.92:((v+0.055)/1.055)**2.4);
 return 0.2126*r+0.7152*g+0.0722*b;
}
function contrast(a,b){const x=luminance(a),y=luminance(b);return (Math.max(x,y)+0.05)/(Math.min(x,y)+0.05);}
test('themes exist, are legible, and separate semantic categories',()=>{
 assert.equal(pkg.contributes.themes.length,3);
 const labels=pkg.contributes.themes.map(t=>t.label);
 assert.deepEqual(labels,['Nova Studio Dark','Nova Studio Light','Nova Aurora Dark']);
 for(const themeInfo of pkg.contributes.themes){
  const t=JSON.parse(fs.readFileSync(path.join(root,themeInfo.path),'utf8'));
  assert.equal(t.name,themeInfo.label);
  const background=t.colors['editor.background'];
  const getScope=(name)=>t.tokenColors.find(c=>[c.scope].flat().includes(name))?.settings.foreground;
  const fn=getScope('entity.name.function.call.nova');
  const ty=getScope('support.type.user.nova');
  const kw=getScope('keyword.control.flow.nova');
  const prop=getScope('variable.other.property.nova');
  assert.equal(new Set([fn,ty,kw,prop]).size,4,'Semantic color hierarchy is distinguishable');
  for(const token of t.tokenColors){
   const color=token.settings.foreground;
   assert.match(color,/^#[0-9A-Fa-f]{6}$/);
   assert.ok(contrast(color,background)>=4.4,`${t.name} ${JSON.stringify(token.scope)} insufficient contrast: ${contrast(color,background).toFixed(2)}`);
  }
 }
});
