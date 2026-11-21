#!/usr/bin/env node
'use strict';
const fs=require('node:fs'),path=require('node:path');
const [command,input]=process.argv.slice(2);
if(command==='lsp-probe'){
  if(process.env.NOVA_MOCK_LEGACY==='1')process.exit(1);
  console.log('nova-lsp-overlays-v2');process.exit(0);
}
const overlays=new Map();
if(process.env.NOVA_LSP_OVERLAY_MAP){
  const records=fs.readFileSync(process.env.NOVA_LSP_OVERLAY_MAP,'utf8').split('\n');
  for(const record of records){const at=record.indexOf('\t');if(at!==-1)overlays.set(record.slice(0,at),record.slice(at+1));}
}
function read(file){return fs.readFileSync(overlays.get(fs.existsSync(file)?fs.realpathSync(file):file)||file,'utf8');}
function diagnostic(text,source){
  const markers=[['BAD_SYNTAX','NOVA-P001','unexpected token BAD_SYNTAX'],['MOVED_OWNER','NOVA-T001','use of moved value'],['BAD_BORROW','NOVA-T001','conflicting mutable borrow'],['BAD_TYPE','NOVA-T001','unknown symbol BAD_TYPE'],['BAD_RAII','NOVA-T001','owned value cannot be overwritten']];
  const matches=markers.map(([token,code,message])=>({token,code,message,at:text.indexOf(token)})).filter(x=>x.at!==-1).sort((a,b)=>a.at-b.at);
  if(!matches.length)return false;
  const m=matches[0], before=text.slice(0,m.at),line=before.split('\n').length,column=before.length-before.lastIndexOf('\n');
  console.error(`error[${m.code}]: ${m.message}\n  --> ${source}:${line}:${column}\n${line} | ${text.split('\n')[line-1]}\n  | ^${'~'.repeat(m.token.length-1)}\n  = compilation phase: ownership`);
  process.exit(1);
}
if(command==='parse-rust'){
  const source=read(input); if(source.includes('BAD_SYNTAX'))diagnostic(source,input);
  console.log('OK '+input);process.exit(0);
}
if(command==='check'||command==='lint'){
  const file=fs.statSync(input).isDirectory()?path.join(input,'src','main.nova'):input;
  const files=[file];
  if(fs.statSync(input).isDirectory()){
    const other=path.join(input,'src','other.nova');if(fs.existsSync(other))files.push(other);
    // Reflect a module target declared in main.nova but not yet saved to disk.
    if(read(file).includes('mod fresh;')){
      const fresh=path.join(input,'src','fresh.nova');
      if(fs.existsSync(fresh)||overlays.has(fresh))files.push(fresh);
    }
  }
  for(const source of files)diagnostic(read(source),source);
  if(command==='lint') {
    for(const source of files){if(read(source).includes('let unused: i32'))console.error("warning[NOVA-L001]: 'unused' in function 'main' is never read");}
    console.log('Nova lint: '+files.length+' warning(s)');
  }else console.log('OK: '+input);
  process.exit(0);
}
if(command==='fmt'){
  const source=read(input);
  fs.writeFileSync(input,source.split('\n').map(s=>s.trimEnd()).join('\n').replace(/([^\n])$/,'$1\n'),'utf8');
  process.exit(0);
}
console.error('unsupported mock command '+command);process.exit(1);
