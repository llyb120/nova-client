import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {AjvJsonSchemaValidator} from '@modelcontextprotocol/sdk/validation/ajv';
const tools=JSON.parse(await readFile(new URL('./chrome-tools.json',import.meta.url),'utf8'));
const byName=Object.fromEntries(tools.map(t=>[t.name,t]));
const validator=new AjvJsonSchemaValidator();
const validates=(name,args)=>validator.getValidator(byName[name].parameters)(args).valid;

test('fourteen narrow Chrome schemas cover the existing executor without employee tools',()=>{
  const cases={
    chrome_tabs:{incognito:false,maxTextChars:0},
    chrome_navigate:{url:'https://example.com',target:{kind:'existing_tab',tabTag:'C1'},settleMs:0},
    chrome_tab_control:{tabTag:'C1',command:'reload'},
    chrome_inspect:{tabTag:'C1',query:'submit',maxTextChars:0},
    chrome_screenshot:{tabTag:'C1',capture:{kind:'viewport',maxEdge:0}},
    chrome_act:{tabTag:'C1',actions:[{action:'hover',ref:'node'},{action:'fill',ref:'input',value:''},{action:'wait',milliseconds:0}]},
    chrome_evaluate:{tabTag:'C1',expression:'1',observe:false},
    chrome_run:{tabTag:'C1',task:'Inspect menu',steps:[{action:'click',target:'menu'}]},
    chrome_connection:{command:'cancel_operation',tabTag:'C1'},
    chrome_downloads:{since:0},
    chrome_devtools:{tabTag:'C1',kind:'network',clear:false},
    chrome_experience_search:{scope:'https://example.com',task:'Open menu'},
    chrome_experience_save:{recordingId:'demo',scope:'https://example.com',task:'Open menu',conditions:['Home'],steps:['Open menu'],checks:['Menu visible'],evidence:'Menu visible',redacted:true},
    chrome_experience_feedback:{tabTag:'C1',snapshotId:'s',scope:'https://example.com',task:'Open menu',routeId:'route',outcome:'success',reason:'Menu visible',evidence:'Menu visible',redacted:true},
  };
  assert.deepEqual(Object.keys(byName).sort(),Object.keys(cases).sort());
  for(const [name,args] of Object.entries(cases)) {
    assert.equal(validates(name,args),true,name);
    assert.equal(validates(name,{...args,operation:'anything'}),false,name+' rejects unrelated fields');
  }
});

test('action/capture branches reject filled placeholders and preserve real zero values',()=>{
  for(const action of [{action:'click',ref:'r',x:0},{action:'hover',ref:'r',text:''},{action:'hover'},{action:'upload',selector:'#f',files:[]}])
    assert.equal(validates('chrome_act',{tabTag:'C1',actions:[action]}),false,JSON.stringify(action));
  assert.equal(validates('chrome_act',{tabTag:'C1',actions:[{action:'click_at',x:0,y:0}]}),true);
  assert.equal(validates('chrome_act',{tabTag:'C1',actions:[{action:'click',target:'登录'}]}),true,'click may use target instead of ref');
  assert.equal(validates('chrome_act',{tabTag:'C1',actions:[{action:'fill',target:'邮箱',value:'a@b.c'}]}),true);
  assert.equal(validates('chrome_act',{tabTag:'C1',actions:[{action:'click'}]}),false);
  for(const maxEdge of [0,320,3840])assert.equal(validates('chrome_screenshot',{tabTag:'C1',capture:{kind:'viewport',maxEdge}}),true);
  for(const maxEdge of [-1,1,319,3841])assert.equal(validates('chrome_screenshot',{tabTag:'C1',capture:{kind:'viewport',maxEdge}}),false);
  assert.equal(validates('chrome_screenshot',{tabTag:'C1',capture:{kind:'region',region:{x:0,y:0,width:0,height:10}}}),false);
  assert.equal(validates('chrome_navigate',{url:'https://example.com',target:{kind:'new_tab',tabTag:''}}),false);
});

test('bundled schemas use only the runtime validator supported vocabulary',()=>{
  const keys=new Set(['type','properties','required','additionalProperties','description','enum','minimum','maximum','exclusiveMinimum','minLength','maxLength','anyOf','minItems','maxItems','items']);
  const visit=s=>{
    for(const key of Object.keys(s))assert(keys.has(key),key);
    if(s.type)assert(['object','string','boolean','integer','number','array'].includes(s.type),s.type);
    for(const child of Object.values(s.properties||{}))visit(child);
    for(const child of s.anyOf||[])visit(child);
    if(s.items)visit(s.items);
  };
  for(const tool of tools)visit(tool.parameters);
});
