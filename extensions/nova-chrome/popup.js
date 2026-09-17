const tag=document.querySelector('#tag'),copy=document.querySelector('#copy'),status=document.querySelector('#status');
async function update(){
  try{
    const result=await chrome.runtime.sendMessage({type:'status'});if(result.error)throw Error(result.error);
    tag.textContent=result.tab?.tag || '没有激活的标签页';copy.disabled=!result.tab;
    status.textContent=result.connection?.connected ? `已连接 ${result.connection.count} 个 Nova` : '等待 Nova 自动连接';
  }catch(error){status.textContent=String(error?.message || error);}
}
copy.onclick=async()=>{try{await navigator.clipboard.writeText(tag.textContent);copy.textContent='已复制';}catch(error){status.textContent=String(error?.message || error);}};
void update();
