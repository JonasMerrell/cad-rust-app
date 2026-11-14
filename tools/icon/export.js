// node export.js <svg> <outdir>: PNGs at the usual icon sizes
const {chromium}=require('playwright');const fs=require('fs');const path=require('path');
(async()=>{const [svgPath,out,smallPath]=process.argv.slice(2);const big=fs.readFileSync(svgPath,'utf8');const sm=smallPath?fs.readFileSync(smallPath,'utf8'):big;
const b=await chromium.launch();
for(const s of [16,24,32,48,64,128,256,512]){const p=await b.newPage({viewport:{width:s,height:s}});
const svg=s<=32?sm:big;await p.setContent(`<html><body style="margin:0;background:transparent">${svg.replace('width="512" height="512"',`width="${s}" height="${s}"`)}</body></html>`);
await p.screenshot({path:path.join(out,`rcad-${s}.png`),omitBackground:true,clip:{x:0,y:0,width:s,height:s}});await p.close();}
await b.close()})()
