import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "node:http";
import { once } from "node:events";
import { spawn } from "node:child_process";
import { gzipSync } from "node:zlib";
import { mkdtemp, mkdir, writeFile, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { createDevinServer } from "./devin-openai.mjs";
import { decode, field as f, proto, envelope, value, string } from "./devin-protocol.mjs";

if (process.argv.includes("--fake-cli")) {
  if (process.argv.includes("version")) console.log("devin 3000.11.205");
  else console.log(JSON.stringify({ families: [{ slug: "test", aliases: [], variants: ["test-model-low","test-model-high","adaptive"].map(model_uid => ({ model_uid, label: model_uid, max_context_tokens: 272000, max_output_tokens: 160000 })) },
    { slug: "fusion", aliases: ["ensemble"], variants: [{ model_uid: "fusion-test-model-high-sidekick-swe-2-medium" }] }] }));
} else {
  test("native Devin adapter protocols and error boundaries", { timeout: 30000 }, async t => {
    const root = await mkdtemp(join(tmpdir(), "nova-devin-native-test-")), config = join(root, "config");
    await mkdir(join(config,"devin"), { recursive: true });
    let latest, disconnected = 0;
    const upstream = createServer(async (req,res) => {
      try {
        const chunks = []; for await (const c of req) chunks.push(c);
        const input = Buffer.concat(chunks); assert.equal(input[0],0);
        const fields = decode(input.subarray(5));
        assert.equal(string(decode(value(fields,1)),3),"fake-upstream-key");
        const messages = fields.filter(([id]) => id===3).map(([,b]) => decode(b));
        const tools = fields.filter(([id]) => id===10).map(([,b]) => decode(b));
        const mode = string(messages.at(-1),3), model = string(fields,21);
        latest = { fields, messages, tools, mode, model };
        res.writeHead(200,{"content-type":"application/connect+proto"});
        const send = (...fields) => res.write(envelope(proto(...fields)));
        const text = value => send(f(3,value));
        const call = (name,args,id="call_1") => { send(f(6,proto(f(1,id),f(2,name)))); const mid=Math.floor(args.length/2); send(f(6,f(3,args.slice(0,mid)))); send(f(6,f(3,args.slice(mid)))); };
        if (["hang","cancel"].includes(mode)) { res.once("close",()=>disconnected++); if(mode==="cancel")text("waiting"); return; }
        if (mode==="error") { res.end(envelope(Buffer.from(JSON.stringify({error:{code:"permission_denied",message:"secret-upstream-token"}})),2)); return; }
        if (mode==="partial") { text("partial");res.end();return; }
        if (mode==="invalid-frame") { res.end(Buffer.from([0,0,0,0,3,255]));return; }
        if (["tool","parallel","bad-args","structured","missing-call"].includes(mode)) send(f(5,10));
        if (mode==="unknown-stop") send(f(5,99));
        if (mode==="inspect") {
          const options=decode(value(fields,8));
          text(JSON.stringify({model,system:string(fields,2),roles:messages.map(m=>value(m,2)),toolId:string(messages.find(m=>value(m,2)===4)||[],7),
            image:messages.some(m=>value(m,10)),temperature:value(options,5).readDoubleLE(),top_p:value(options,8).readDoubleLE(),max_tokens:value(options,2),tools:tools.map(tool=>string(tool,1))}));
        } else if (mode==="structured") { text("Do not leak this preamble");call("__nova_structured_response",'{"result":{"city":"上海"}}'); }
        else if (["tool","parallel","bad-args"].includes(mode)) {
          call("get_weather",mode==="bad-args"?'{"city":42}':'{"city":"上海"}');
          if(mode==="parallel")call("get_weather",'{"city":"北京"}',"call_2");
        } else if (mode==="stop") { text("hello EN");text("D must not be returned"); }
        else if (mode==="gzip") res.write(envelope(gzipSync(proto(f(3,"压缩成功"))),1));
        else { text("你");text("好"); }
        if(mode==="length")send(f(5,3));
        send(f(7,proto(f(2,20),f(3,8),f(5,5))));
        const end=envelope(Buffer.from("{}"),2);res.write(end.subarray(0,3));setImmediate(()=>res.end(end.subarray(3)));
      } catch { res.destroy(); }
    });
    upstream.listen(0,"127.0.0.1");await once(upstream,"listening");
    const credentialText=`windsurf_api_key = "fake-upstream-key"\napi_server_url = "http://127.0.0.1:${upstream.address().port}"\nrefresh_token = "untouched"\n`;
    await writeFile(join(config,"devin/credentials.toml"),credentialText);
    await writeFile(join(config,"devin/config.json"),'{"agent":{"model":"test-model-low"},"hooks":["never-run"]}');
    const env={...process.env,APPDATA:config,XDG_CONFIG_HOME:config,DEVIN_CONFIG_HOME:config};delete env.DEVIN_MODEL;
    const server=createDevinServer({apiKey:"test-key",binary:process.execPath,prefixArgs:[fileURLToPath(import.meta.url),"--fake-cli"],env,timeoutMs:1200,maxConcurrent:1});
    server.listen(0,"127.0.0.1");await once(server,"listening");
    const url=`http://127.0.0.1:${server.address().port}`,headers={authorization:"Bearer test-key","content-type":"application/json"};
    const post=(content,extra={},options={})=>fetch(`${url}/v1/chat/completions`,{method:"POST",headers,body:JSON.stringify({model:"test-model-low",messages:[{role:"user",content}],...extra}),...options});
    const responses=body=>fetch(`${url}/v1/responses`,{method:"POST",headers,body:JSON.stringify({model:"test-model-low",...body})});
    const tools=[{type:"function",function:{name:"get_weather",description:"Weather",parameters:{type:"object",properties:{city:{type:"string"}},required:["city"],additionalProperties:false},strict:true}}];
    const sse=wire=>wire.trim().split("\n\n").filter(x=>x.includes("data: ")).map(x=>x.split("data: ")[1]).filter(x=>x!=="[DONE]").map(JSON.parse);
    t.after(async()=>{server.shutdown();upstream.close();upstream.closeAllConnections();if(resolve(root).startsWith(resolve(tmpdir())+sep))await rm(root,{recursive:true,force:true,maxRetries:3});});

    await t.test("auth/catalog and trust-boundary validation",async()=>{
      assert.equal((await fetch(`${url}/v1/models`)).status,401);
      const catalog=await(await fetch(`${url}/v1/models`,{headers})).json();assert.equal(catalog.data[0].id,"devin");
      assert.deepEqual(catalog.data.map(model=>model.id),["devin","test-model-low","test-model-high","adaptive"]);
      for(const model of ["fusion","ensemble","fusion-test-model-high-sidekick-swe-2-medium"]){
        const rejected=await post("hello",{model});assert.equal(rejected.status,400);assert.equal((await rejected.json()).error.code,"model_not_found");
        assert.equal((await responses({model,input:"hello"})).status,400);
      }
      assert.equal(catalog.data[1].context_length,272000);assert.equal(catalog.data[1].top_provider.max_completion_tokens,128000);assert.deepEqual(catalog.data[1].reasoning_options,[]);
      for(const prompt_cache_key of [42,"x".repeat(65)])assert.equal((await post("hello",{prompt_cache_key})).status,400);
      for(const extra of [{temperature:3},{max_tokens:1},{tools:[{type:"function",function:{}}]},{n:5},{messages:[]},{stream:"true"},{tool_choice:"required"},{tools,tool_choice:{type:"function",function:{name:"missing"}}},{stream:true,stream_options:{include_usage:"true"}}])assert.equal((await post("hello",extra)).status,400);
      assert.equal((await post("hello",{},{body:"{"})).status,400);
      assert.equal((await post("hello",{},{headers:{authorization:headers.authorization}})).status,415);
      assert.equal((await responses({input:"hello",background:true})).status,400);
      for (const extra of [{tools:{}},{reasoning:"high"},{store:1},{text:[]}]) assert.equal((await responses({input:"hello",...extra})).status,400);
    });
    await t.test("native roles, images, model/effort and generation parameters",async()=>{
      const messages=[{role:"system",content:"SYSTEM"},{role:"user",content:"weather"},{role:"assistant",content:null,tool_calls:[{id:"old-call",type:"function",function:{name:"get_weather",arguments:'{"city":"上海"}'}}]},{role:"tool",tool_call_id:"old-call",content:"sunny"},{role:"user",content:[{type:"image_url",image_url:{url:"data:image/png;base64,YQ=="}},{type:"text",text:"inspect"}]}];
      const r=await post("inspect",{model:"devin",reasoning_effort:"high",messages,tools,temperature:0.3,top_p:0.8,max_tokens:96});assert.equal(r.status,200);
      const result=await r.json(),details=JSON.parse(result.choices[0].message.content);
      assert.deepEqual(details.roles,[1,2,4,1]);assert.equal(details.system,"SYSTEM");assert.equal(details.toolId,"old-call");assert.equal(details.model,"test-model-high");assert.equal(details.image,true);assert.equal(details.temperature,0.3);assert.equal(details.top_p,0.8);assert.equal(details.max_tokens,96);
      assert.deepEqual(details.tools,["get_weather"]);assert.equal(result.usage.total_tokens,28);assert.equal(result.usage.prompt_tokens_details.cached_tokens,5);
      assert.equal(await readFile(join(config,"devin/credentials.toml"),"utf8"),credentialText);
      for(const image of ["file:///secret","https://127.0.0.1/image.png","https://[::1]/x","data:image/png;base64,!!"]){assert.equal((await post([{type:"image_url",image_url:{url:image}}])).status,400);}
    });
    await t.test("native function calls, parallel calls and tool result continuation",async()=>{
      const result=await(await post("parallel",{tools,tool_choice:"required"})).json();assert.equal(result.choices[0].finish_reason,"tool_calls");assert.equal(result.choices[0].message.tool_calls.length,2);
      const assistant=result.choices[0].message;
      const messages=[{role:"user",content:"parallel"},assistant,...assistant.tool_calls.map(call=>({role:"tool",tool_call_id:call.id,content:"sunny"}))];
      const follow=await post("",{messages,tools,tool_choice:"none"});assert.equal(follow.status,200);assert.deepEqual(latest.tools,[]);assert.equal(value(latest.messages.at(-1),2),4);
      assert.equal((await post("",{messages:messages.slice(0,-1),tools})).status,400);
      for(const [mode,extra]of[["tool",{tools,tool_choice:"none"}],["hello",{tools,tool_choice:"required"}],["bad-args",{tools}],["parallel",{tools,parallel_tool_calls:false}]])assert.equal((await post(mode,extra)).status,502);
    });
    await t.test("streaming tool deltas and real usage",async()=>{
      const response=await post("tool",{tools:tools.map(tool=>({...tool,function:{...tool.function,strict:false}})),stream:true,stream_options:{include_usage:true},max_completion_tokens:32000,prompt_cache_key:"lyra-session"});assert.equal(response.status,200);
      const wire=await response.text();assert.match(wire,/data: \[DONE\]/);
      const events=sse(wire),deltas=events.flatMap(event=>event.choices.flatMap(choice=>choice.delta?.tool_calls||[]));
      assert.equal(deltas[0].id,"call_1");assert.equal(deltas[0].function.name,"get_weather");assert.equal(deltas.map(delta=>delta.function.arguments).join(""),'{"city":"上海"}');
      assert.equal(events.at(-2).choices[0].finish_reason,"tool_calls");assert.equal(events.at(-1).usage.total_tokens,28);
    });
    await t.test("JSON schema, stop across chunks, gzip and multiple choices",async()=>{
      const response_format={type:"json_schema",json_schema:{name:"answer",strict:true,schema:{type:"object",properties:{city:{type:"string"}},required:["city"],additionalProperties:false}}};
      const result=await(await post("structured",{response_format})).json();assert.equal(result.choices[0].message.content,'{"city":"上海"}');assert.equal(result.choices[0].message.tool_calls,undefined);
      assert.equal(result.choices[0].finish_reason,"stop");
      const wire=await(await post("structured",{response_format,stream:true})).text();assert.doesNotMatch(wire,/preamble|__nova_structured/);
      assert.equal(sse(wire).at(-1).choices[0].finish_reason,"stop");
      const stopped=await(await post("stop",{stop:["END"]})).json();assert.equal(stopped.choices[0].message.content,"hello ");
      assert.equal((await(await post("gzip")).json()).choices[0].message.content,"压缩成功");
      assert.equal((await(await post("hello",{n:2})).json()).choices.length,2);
      assert.equal((await(await post("length")).json()).choices[0].finish_reason,"length");
    });
    await t.test("Responses function calls, previous_response_id and SSE lifecycle",async()=>{
      const flatTools=tools.map(tool=>({type:"function",...tool.function}));
      const first=await(await responses({input:"tool",tools:flatTools,tool_choice:"required",instructions:"CURRENT"})).json();assert.equal(first.output[0].type,"function_call");assert.equal(first.output[0].call_id,"call_1");
      const next=await responses({previous_response_id:first.id,input:[{type:"function_call_output",call_id:"call_1",output:"sunny"}],tools:flatTools,tool_choice:"none"});assert.equal(next.status,200);assert.equal(string(latest.fields,2),"");assert.equal(value(latest.messages.at(-1),2),4);
      assert.equal((await responses({previous_response_id:"unknown",input:"hello"})).status,404);
      const wire=await(await responses({input:"tool",tools:flatTools,stream:true})).text(),events=sse(wire);
      for(const type of["response.created","response.output_item.added","response.function_call_arguments.delta","response.function_call_arguments.done","response.output_item.done","response.completed"])assert.ok(events.some(event=>event.type===type),type);
      assert.equal(events.at(-1).response.output[0].arguments,'{"city":"上海"}');assert.equal(events.at(-1).response.usage.total_tokens,28);
      assert.deepEqual(events.map(event=>event.sequence_number),events.map((_,i)=>i));assert.doesNotMatch(wire,/\[DONE\]/);
      const textEvents=sse(await(await responses({input:"hello",stream:true})).text());assert.ok(textEvents.some(event=>event.type==="response.output_text.done"&&event.text==="你好"));
    });
    await t.test("partial/errors cannot masquerade as successful streams",async()=>{
      const error=await post("error");assert.equal(error.status,403);assert.doesNotMatch(await error.text(),/secret-upstream-token/);
      for(const mode of["partial","invalid-frame","missing-call","unknown-stop"])assert.equal((await post(mode)).status,502);
      const wire=await(await post("partial",{stream:true})).text();assert.match(wire,/"error"/);assert.doesNotMatch(wire,/\[DONE\]|"finish_reason":"stop"/);
      const events=sse(await(await responses({input:"partial",stream:true})).text());assert.equal(events.at(-1).type,"error");assert.ok(!events.some(e=>e.type==="response.completed"));
      for(const mode of["missing-call","unknown-stop"]){
        const wire=await(await post(mode,{stream:true})).text();assert.match(wire,/"error"/);assert.doesNotMatch(wire,/\[DONE\]/);
        const events=sse(await(await responses({input:mode,stream:true})).text());assert.equal(events.at(-1).type,"error");assert.ok(!events.some(e=>e.type==="response.completed"));
      }
      assert.throws(()=>decode(Buffer.from([10,5,1])));assert.throws(()=>decode(Buffer.from([128])));
    });
    await t.test("managed startup needs no connection file and exits with its owner",async()=>{
      const data=join(root,"managed-nova");
      const child=spawn(process.execPath,[fileURLToPath(new URL("./devin-openai.mjs",import.meta.url)),"--managed"],{
        env:{...process.env,NOVA_DATA_DIR:data,NOVA_DEVIN_PORT:"invalid",NOVA_DEVIN_HOST:"0.0.0.0"},windowsHide:true,stdio:["pipe","pipe","pipe"]});
      try {
        const [output]=await once(child.stdout,"data",{signal:AbortSignal.timeout(10000)}),connection=JSON.parse(output.toString());
        assert.match(connection.baseURL,/^http:\/\/127\.0\.0\.1:\d+\/v1$/);assert.match(connection.apiKey,/^[a-f0-9]{48}$/);
        await assert.rejects(readFile(join(data,"alkaid/devin-api.json")),{code:"ENOENT"});
        assert.equal((await fetch(`${connection.baseURL}/unknown`,{headers:{authorization:`Bearer ${connection.apiKey}`}})).status,404);
        const exited=once(child,"exit",{signal:AbortSignal.timeout(10000)});child.stdin.end();assert.equal((await exited)[0],0);
      } finally { if(child.exitCode===null&&child.signalCode===null){const exited=once(child,"exit");child.kill();await exited;} }
    });
    await t.test("CLI publishes and reuses local credentials without user input",async()=>{
      const listener=createServer().listen(0,"127.0.0.1");await once(listener,"listening");
      const port=listener.address().port;await new Promise(resolve=>listener.close(resolve));
      const data=join(root,"nova"),childEnv={...process.env,NOVA_DATA_DIR:data,NOVA_DEVIN_HOST:"127.0.0.1",NOVA_DEVIN_PORT:String(port)};
      delete childEnv.NOVA_DEVIN_API_KEY;
      let previousKey;
      for(let attempt=0;attempt<2;attempt++){
        const child=spawn(process.execPath,[fileURLToPath(new URL("./devin-openai.mjs",import.meta.url))],{env:childEnv,windowsHide:true,stdio:["ignore","pipe","pipe"]});
        try {
          const [output]=await once(child.stdout,"data",{signal:AbortSignal.timeout(10000)});
          const connection=JSON.parse(await readFile(join(data,"alkaid/devin-api.json"),"utf8"));
          assert.equal(connection.baseURL,`http://127.0.0.1:${port}/v1`);assert.match(connection.apiKey,/^[a-f0-9]{48}$/);
          assert.ok(!output.toString().includes(connection.apiKey));
          if(previousKey)assert.equal(connection.apiKey,previousKey);previousKey=connection.apiKey;
          assert.equal((await fetch(`${connection.baseURL}/unknown`,{headers:{authorization:`Bearer ${connection.apiKey}`}})).status,404);
          assert.equal((await fetch(`${connection.baseURL}/unknown`)).status,401);
        } finally { if(child.exitCode===null&&child.signalCode===null){const exited=once(child,"exit");child.kill();await exited;} }
      }
    });
    await t.test("disconnect, concurrency limit and timeout cancel upstream work",async()=>{
      const abort=new AbortController();const response=await post("cancel",{stream:true},{signal:abort.signal});
      assert.equal((await post("busy")).status,429);const reader=response.body.getReader();await reader.read();abort.abort();await reader.cancel().catch(()=>{});
      for(let i=0;i<50&&!disconnected;i++)await new Promise(r=>setTimeout(r,20));assert.ok(disconnected);
      assert.equal((await post("hang")).status,504);assert.equal((await post("hello")).status,200);
    });
  });
}
